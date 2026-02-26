# luze

A digital note box following Niklas Luhmann's Zettelkasten method.

Notes use Luhmann-style IDs (`1a1`, `1a2`, `1b`, ...) that encode their position in a tree. Content never gets overwritten — updates archive the old version as a child note. Notes are stored as JSON in per-drawer files, lazily loaded.

## Usage

```
zk init
zk add 1 "First note"
zk add 1a "A thought branching off"
zk update 1 "Revised first note"
zk tree
```

Set `ZK_PATH` to change the storage directory (default: `./.zk`).

Run `zk help` for all commands.

## License

Apache-2.0 OR MIT
