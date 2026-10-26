# The brain format is versioned brain-wide and migrated on the way into the index

The brain folder records one format version in a committed `brain.toml`, and a newer binary migrates an older brain automatically at startup. Migrations are idempotent per-file transforms applied to every file as it enters the index, not a one-off pass triggered by the version number. With several machines sharing a brain through git, a machine still on an old binary can write old-shape files offline and later rebase them onto a brain another machine has already migrated; the version then says "current" while some files are not, and only migrating on the way in catches them. An older binary that meets a newer format serves every tool as `incompatible` and does not push. We accept that one machine's upgrade locks the others out until they upgrade too, in exchange for only ever holding the current format in memory.

## Considered Options

- **A version in every file:** catches mixed brains directly, but every file carries noise and every migration rewrites every file's header.
- **An explicit `brain migrate` command:** the owner controls when other machines are locked out, but the binary must read every older format until then.
