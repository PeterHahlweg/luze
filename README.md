# luze

A digital Zettelkasten following Niklas Luhmann's method.

Notes use Luhmann-style IDs (`1a1`, `1a2`, `1b`, ...) that encode their position in a branching tree. Notes are **immutable** — content is never overwritten. To refine a thought, use `update`, which creates a new child note that supersedes the original; existing children stay in place. Notes are stored as JSON in per-drawer files, lazily loaded.

## Usage

```
luze init
luze add 1 "First note"
luze add 1a "A thought branching off"
luze update 1 "A refined version of the first note"
luze tree
luze sync -m "refine thought"
```

Set `LUZE_PATH` to change the storage directory (default: `./.luze`).

Run `luze help` for all commands.

## Sync

`luze sync` lets multiple people share a Zettelkasten over a git remote. This command commits local work, pulls, resolves any conflicts, and pushes — no manual `git mergetool` needed.

Conflicts in draw files are resolved automatically at the note level:

- **Note exists only on one side** — added as-is.
- **Same ID, same content, different links** — link lists are unioned.
- **Same ID, different content** — two collaborators independently wrote a note at the same position. Both are kept; the local one is moved to the next available sibling ID.

Because notes are immutable, the third case is stable after one sync: once both versions exist in the shared history, the same conflict will never recur.

## License

Apache-2.0 OR MIT
