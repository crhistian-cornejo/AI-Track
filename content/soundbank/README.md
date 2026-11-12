# AI-Track sound bank

AI-Track ships with a factory library of free sounds, and it lists more in a
**Content Hub** that the user downloads from the official pages. This folder
holds the manifest that drives both, plus the attribution file. The audio is
not in git.

| Path | What it is | In git |
|---|---|---|
| `manifest.toml` | Every pack and plugin, with its license and where we checked it | yes |
| `ATTRIBUTION.md` | Credits for the bundled packs, generated from the manifest | yes |
| `packs/<id>/` | Downloaded audio, with `LICENSE`, `SOURCE.txt` and `.files.tsv` for each pack | no |
| `.cache/` | Downloaded archives | no |
| `factory.db` | Index of `packs/`, built by `aitrack-library` | no |

## License policy

A pack goes into the bundled bank (`[[pack]]`) only when its license
explicitly allows redistribution inside another free product:

- **CC0-1.0**: public domain dedication.
- **CC-BY-3.0 / CC-BY-4.0**: allowed with attribution. The credit line goes
  into `ATTRIBUTION.md` and the app's about screen.
- **Explicit written permission** on the official page. None of the current
  packs relies on this.

Everything else goes to the Content Hub (`[[hub]]`) as a link only:

- "Royalty-free for your music" freebies (Cymatics, Splice, 99Sounds, BPB,
  Producer Spot, Looperman, BandLab, Sonniss GDC, etc.). Their terms allow
  use in your tracks but forbid redistributing the files.
- Packs whose license is unclear or whose provenance we could not verify,
  for example the Fischer TR-808 set and the Clipabay kit.
- Share-alike, non-commercial or GPL sample licenses, for example Virtual
  Playing Orchestra.
- CC0 or CC-BY libraries that are too large to bundle (Virtuosity Drums,
  DrumGizmo kits, the full VSCO 2 CE and VCSL). These are offered as optional
  downloads.
- Free plugins (Vital, Surge XT, Dexed, Valhalla Supermassive, etc.). They
  are never bundled. The hub links to the official download page and lists
  the license, formats and operating systems.

A `[[pack]]` with `optional = true` is redistributable but stays out of the
default bank because of its size. Today that's the Salamander Grand Piano
(CC BY 3.0, 1.15 GB of WAV). It goes through the same verified pipeline when
the user asks for it.

`aitrack_library::hub::Manifest::validate` enforces the rule: a `[[pack]]`
with a license outside the allowed list, or a CC-BY pack without an
attribution, fails to parse. That check runs both in the build and in
`cargo xtask`.

Each `[[pack]]` records `license_evidence`, which says where the license was
read (the license file in the repository or archive, or the official page)
and quotes it. When in doubt, a pack stays in the hub.

## Commands

```sh
cargo xtask soundbank list                    # packs, sizes, licenses, fetched or not
cargo xtask soundbank fetch                   # download all, verify, extract, index
cargo xtask soundbank fetch --only salamander-grand-piano --max-size-mb 1500
cargo xtask soundbank fetch --with-optional   # also the optional packs (Salamander piano, 1.15 GB)
cargo xtask soundbank fetch --write-manifest  # also store sha256 + sample_count in manifest.toml
cargo xtask soundbank verify                  # re-check hashes, LICENSE files, file lists
cargo xtask soundbank index                   # rebuild factory.db from packs/
cargo xtask soundbank attribution             # regenerate ATTRIBUTION.md
```

Packs are fetched one at a time:

- **Archives** (zip, tar.gz, tar.xz, 7z) are downloaded to `.cache/<id>.<ext>.part`
  and renamed only when complete. The sha256 is checked against the manifest
  before extraction.
- **`github_tree` packs** take only the selected folders of a repository at a
  pinned commit. Each file is checked against its git blob hash. The manifest's
  `sha256` covers the sorted file list, so an upstream change is detected.
  An interrupted run resumes and skips files that are already correct.

Extraction is pure Rust (`zip`, `tar` + `flate2`, `lzma-rs`, `sevenz-rust2`).
Unsafe paths are rejected. macOS metadata (`__MACOSX`, `._*`) and the
`exclude` patterns are dropped.

Each pack's own license text is copied next to it as `LICENSE`. When the pack
doesn't ship one, `LICENSE` is a notice with the SPDX id, URL and evidence.
`SOURCE.txt` records the source URL, hash and attribution.

## Factory library

`factory.db` uses a **relative** root (`packs`), resolved against the
database's own folder. The app can install `factory.db` and `packs/` anywhere
side by side. Pack metadata (license, author, default category, genres) is
written to the `packs` table from the manifest, so samples with
uninformative names, such as multisampled instruments, still land in the
right virtual folder.

Sample labels come from whole words in the file name and its folders inside
the pack; the pack folder's own name (`karoryfer-string-cyborgs`) is not read
as a label. A pack's `genres` describe the pack, so they reach its samples only
when the pack lists exactly one. When those rules change,
`aitrack_library::reanalyse::TAGGER_VERSION` goes up and every library,
`factory.db` included, re-derives its labels the next time it is opened.

## Adding a pack

1. Read the license on the official page **and** in the archive or repository.
   Quote it in `license_evidence`.
2. Add a `[[pack]]` only if it is CC0, CC-BY or explicitly redistributable.
   Otherwise add a `[[hub]]` entry.
3. Run `cargo xtask soundbank fetch --only <id> --write-manifest` to pin the hash.
4. Run `cargo xtask soundbank attribution` and commit `manifest.toml` and
   `ATTRIBUTION.md`.
