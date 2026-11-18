//! What an external agent's tool calls do to the project, on the UI thread.
//!
//! The same tools as the in-app assistant run against a [`Session`], a draft
//! copy of the project that records the `Command`s. Edits are never applied
//! to the real project by the agent: they are offered as one proposal that
//! the user accepts (one undo step) or discards. Further calls keep building
//! on the draft until the user resolves the proposal; the proposal is
//! replaced by a bigger one each time. With "apply external edits directly"
//! each call is applied at once as its own undo step, except destructive
//! ones, which are always offered.

use aitrack_ai::llm::tools::{self, Outcome, Session};
use aitrack_model::{History, Origin, Project, TrackId};
use serde_json::Value;

/// A proposal the user has to answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub id: u64,
    /// The proposal this one grew out of (now gone), so the chat card can be replaced.
    pub replaces: Option<u64>,
    pub label: String,
    /// One line per change.
    pub summary: Vec<String>,
    pub destructive: bool,
    /// The agent that made it.
    pub origin: String,
}

/// What the call did to the project, for the app to show.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    None,
    Offered(Offer),
    /// Applied at once as one undo step.
    Applied { label: String, origin: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub text: String,
    pub is_error: bool,
    pub effect: Effect,
}

impl Reply {
    fn read(text: String, is_error: bool) -> Self {
        Self { text, is_error, effect: Effect::None }
    }
}

/// Runs the tools the broker does not know (`generate_part` needs the app's pattern engine).
pub type Delegate<'a> = dyn FnMut(&mut Session, &Value) -> Result<String, String> + 'a;

struct Live {
    session: Session,
    proposal: u64,
    origin: String,
}

#[derive(Default)]
pub struct Broker {
    live: Option<Live>,
    /// "Apply external edits directly".
    pub apply_directly: bool,
}

impl Broker {
    pub fn new(apply_directly: bool) -> Self {
        Self { live: None, apply_directly }
    }

    /// The proposal waiting on the user, if the draft is still alive.
    pub fn pending(&self, history: &History) -> Option<u64> {
        // Matched by origin too, so a different project's proposal with the same number is not ours.
        let l = self.live.as_ref()?;
        history.proposals().iter().find(|p| p.id == l.proposal && p.origin == Origin::Agent(l.origin.clone())).map(|p| p.id)
    }

    /// The draft ended with the proposal: the user accepted or discarded it.
    fn settle(&mut self, history: &History) {
        if self.live.is_some() && self.pending(history).is_none() {
            self.live = None;
        }
    }

    /// Forget the draft (the server stopped or the project was replaced).
    pub fn reset(&mut self, history: &mut History) {
        if let Some(l) = self.live.take() {
            history.discard(l.proposal);
        }
    }

    /// Run `name` for `client` on the open project.
    #[allow(clippy::too_many_arguments)]
    pub fn call(&mut self, project: &mut Project, history: &mut History, selected: Option<TrackId>, client: &str, name: &str, input: &Value, delegate: &mut Delegate<'_>) -> Reply {
        self.settle(history);
        let run = |s: &mut Session, delegate: &mut Delegate<'_>| match tools::execute(s, name, input) {
            Outcome::Ok(t) => (t, false),
            Outcome::Err(e) => (e, true),
            Outcome::Delegate => match delegate(s, input) {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            },
        };

        if tools::is_read_only(name) {
            // Reads see the draft when there is one, else the project itself.
            return match &mut self.live {
                Some(l) => {
                    l.session.selected = selected;
                    let (t, e) = run(&mut l.session, delegate);
                    Reply::read(t, e)
                }
                None => {
                    let mut s = Session::new(project, selected);
                    let (t, e) = run(&mut s, delegate);
                    Reply::read(t, e)
                }
            };
        }

        let mut session = match self.live.take() {
            Some(l) => (l.session, Some((l.proposal, l.origin))),
            None => (Session::new(project, selected), None),
        };
        session.0.selected = selected;
        let before = session.0.commands.len();
        let (text, is_error) = run(&mut session.0, delegate);
        let (draft, kept) = session;
        if is_error || draft.commands.len() == before {
            // Nothing changed: put the draft back as it was.
            if let Some((proposal, origin)) = kept {
                self.live = Some(Live { session: draft, proposal, origin });
            }
            return Reply { text, is_error, effect: Effect::None };
        }
        // The draft's ids stay reserved, so the user's own edits meanwhile cannot take them.
        project.ids.observe(draft.draft.ids.last());
        let origin = kept.as_ref().map_or_else(|| client.to_owned(), |(_, o)| o.clone());
        let replaces = kept.map(|(p, _)| p);
        if let Some(old) = replaces {
            history.discard(old);
        }
        let n = draft.changes.len();
        let label = if n == 1 { draft.changes[0].clone() } else { format!("{n} changes by {origin}") };
        let destructive = draft.is_destructive();
        let commands = draft.commands.clone();
        let id = match history.propose(project, label.clone(), Origin::Agent(origin.clone()), commands) {
            Ok(id) => id,
            Err(e) => {
                return Reply {
                    text: format!("The project changed while you were editing, so these edits no longer fit ({e}). Read the project again and repeat them."),
                    is_error: true,
                    effect: Effect::None,
                };
            }
        };
        if self.apply_directly && !destructive {
            return match history.accept(project, id) {
                Ok(true) => {
                    Reply { text: format!("{text}\nApplied to the project (one undo step in AI-Track)."), is_error: false, effect: Effect::Applied { label, origin } }
                }
                Ok(false) | Err(_) => Reply { text: "The edit could not be applied; the project changed meanwhile.".into(), is_error: true, effect: Effect::None },
            };
        }
        let summary = draft.changes.clone();
        self.live = Some(Live { session: draft, proposal: id, origin: origin.clone() });
        let why = if destructive { " It deletes something, so it always needs the user's approval." } else { "" };
        Reply {
            text: format!("{text}\nThis is a draft: the user has to accept it in AI-Track before the project changes.{why} You can keep editing; reads show the draft."),
            is_error: false,
            effect: Effect::Offered(Offer { id, replaces, label, summary, destructive, origin }),
        }
    }

    /// A text description of the edits waiting for the user (for the resource).
    pub fn describe_pending(&self, history: &History) -> String {
        match (&self.live, self.pending(history)) {
            (Some(l), Some(id)) => {
                let mut s = format!("Proposal #{id} by {} waits for the user in AI-Track ({} change(s)):\n", l.origin, l.session.changes.len());
                for c in &l.session.changes {
                    s.push_str("- ");
                    s.push_str(c);
                    s.push('\n');
                }
                s
            }
            _ => "No external edits are waiting.".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_delegate(_: &mut Session, _: &Value) -> Result<String, String> {
        Err("no pattern engine in tests".into())
    }

    fn setup() -> (Project, History, Broker) {
        (Project::new("t"), History::new(100), Broker::default())
    }

    fn call(b: &mut Broker, p: &mut Project, h: &mut History, name: &str, input: Value) -> Reply {
        b.call(p, h, None, "Claude Desktop", name, &input, &mut no_delegate)
    }

    #[test]
    fn an_edit_becomes_a_proposal_and_leaves_the_project_alone() {
        let (mut p, mut h, mut b) = setup();
        let before = p.tracks.len();
        let r = call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass", "kind": "midi" }));
        assert!(!r.is_error, "{}", r.text);
        let Effect::Offered(offer) = r.effect else { panic!("{:?}", r.effect) };
        assert_eq!(offer.origin, "Claude Desktop");
        assert!(!offer.destructive);
        assert_eq!(p.tracks.len(), before, "nothing changes until the user accepts");
        assert_eq!(h.proposals().len(), 1);
        assert!(!h.can_undo());
        assert!(r.text.contains("accept"));
    }

    #[test]
    fn later_calls_build_on_the_draft_and_replace_the_proposal() {
        let (mut p, mut h, mut b) = setup();
        let Effect::Offered(first) = call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" })).effect else { panic!() };
        // A read sees the draft's track; the project has none.
        let proj = call(&mut b, &mut p, &mut h, "get_project", json!({}));
        assert!(proj.text.contains("\"Bass\""), "{}", proj.text);
        assert!(p.tracks.iter().all(|t| t.name != "Bass"));
        // An edit on the track created by the draft works and grows the one proposal.
        let r = call(&mut b, &mut p, &mut h, "add_device", json!({ "track": "Bass", "plugin": "Compressor" }));
        assert!(!r.is_error, "{}", r.text);
        let Effect::Offered(second) = r.effect else { panic!() };
        assert_eq!(second.replaces, Some(first.id));
        assert_eq!(second.summary.len(), 2);
        assert_eq!(h.proposals().len(), 1, "one proposal, replaced");
        assert_eq!(h.proposals()[0].id, second.id);
    }

    #[test]
    fn accepting_is_one_undo_step_and_the_next_call_starts_fresh() {
        let (mut p, mut h, mut b) = setup();
        call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" }));
        let Effect::Offered(o) = call(&mut b, &mut p, &mut h, "add_device", json!({ "track": "Bass", "plugin": "Compressor" })).effect else { panic!() };
        let tracks = p.tracks.len();
        assert!(h.accept(&mut p, o.id).unwrap());
        assert_eq!(p.tracks.len(), tracks + 1);
        assert!(p.tracks.iter().any(|t| t.name == "Bass" && t.devices.len() == 2));
        assert_eq!(b.pending(&h), None);
        // Both edits were one history step.
        assert!(h.undo(&mut p).unwrap());
        assert_eq!(p.tracks.len(), tracks);
        assert!(!h.can_undo());
        // A new edit is a new draft that starts from the project.
        let Effect::Offered(n) = call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Lead" })).effect else { panic!() };
        assert_eq!(n.replaces, None);
        assert_eq!(n.summary.len(), 1);
    }

    #[test]
    fn discarding_drops_the_draft() {
        let (mut p, mut h, mut b) = setup();
        let Effect::Offered(o) = call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" })).effect else { panic!() };
        assert!(h.discard(o.id));
        let proj = call(&mut b, &mut p, &mut h, "get_project", json!({}));
        assert!(!proj.text.contains("\"Bass\""), "the discarded draft is gone: {}", proj.text);
        assert!(h.proposals().is_empty() && !h.can_undo());
    }

    #[test]
    fn applying_directly_is_one_undo_step_per_call_but_destructive_edits_wait() {
        let (mut p, mut h, mut b) = setup();
        b.apply_directly = true;
        let r = call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" }));
        assert!(matches!(r.effect, Effect::Applied { .. }), "{:?}", r.effect);
        assert!(p.tracks.iter().any(|t| t.name == "Bass"));
        assert!(h.can_undo() && h.proposals().is_empty());
        // The next call sees the applied track and applies as its own step.
        let r = call(&mut b, &mut p, &mut h, "add_device", json!({ "track": "Bass", "plugin": "Compressor" }));
        assert!(matches!(r.effect, Effect::Applied { .. }), "{}", r.text);
        assert!(h.undo(&mut p).unwrap());
        assert!(p.tracks.iter().any(|t| t.name == "Bass" && t.devices.len() == 1));
        assert!(h.redo(&mut p).unwrap());
        // Deleting always needs the user.
        let r = call(&mut b, &mut p, &mut h, "delete_track", json!({ "track": "Bass" }));
        let Effect::Offered(o) = r.effect else { panic!("{}", r.text) };
        assert!(o.destructive);
        assert!(p.tracks.iter().any(|t| t.name == "Bass"), "still there");
        assert!(r.text.contains("approval"));
    }

    #[test]
    fn a_failed_edit_changes_nothing() {
        let (mut p, mut h, mut b) = setup();
        let r = call(&mut b, &mut p, &mut h, "add_device", json!({ "track": "Nope", "plugin": "EQ" }));
        assert!(r.is_error && r.effect == Effect::None);
        assert!(h.proposals().is_empty());
        assert!(call(&mut b, &mut p, &mut h, "no_such_tool", json!({})).is_error);
    }

    #[test]
    fn the_project_changing_underneath_is_reported() {
        let (mut p, mut h, mut b) = setup();
        call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" }));
        // The user edits the same ids meanwhile.
        let id = p.alloc_track_id();
        let t = aitrack_model::Track::new(id, aitrack_model::TrackKind::Instrument, "Mine", aitrack_model::Color::palette(1));
        let index = p.tracks.len() - 1;
        h.apply(&mut p, "Add track", Origin::User, vec![aitrack_model::Command::AddTrack { track: Box::new(t), index }]).unwrap();
        // The draft still fits (ids were reserved), and the proposal is replaced.
        let r = call(&mut b, &mut p, &mut h, "add_device", json!({ "track": "Bass", "plugin": "EQ" }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(h.proposals().len(), 1);
        let o = h.proposals()[0].id;
        assert!(h.accept(&mut p, o).unwrap());
        assert!(p.tracks.iter().any(|t| t.name == "Mine") && p.tracks.iter().any(|t| t.name == "Bass"));
    }

    #[test]
    fn describes_what_waits() {
        let (mut p, mut h, mut b) = setup();
        assert!(b.describe_pending(&h).contains("No external"));
        call(&mut b, &mut p, &mut h, "create_track", json!({ "name": "Bass" }));
        assert!(b.describe_pending(&h).contains("waits for the user"));
        b.reset(&mut h);
        assert!(h.proposals().is_empty());
    }
}
