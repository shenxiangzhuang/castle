# Desktop app storage

Status: accepted

## Decision

One app-level SQLite WAL database at `<data-root>/app.sqlite3` stores the
project registry, UI preferences, provider and model catalogs, credentials, model preferences, and
new-session defaults. Under the [core architecture](overview.md), shared product configuration
and persistence belong to the harness; desktop owns the meaning and editing of UI preferences.
Credentials and UI preferences never enter the SDK's session history. The database implementation
lives in `harness`; the storage layout is unchanged.

Every project has a deterministic data directory at `<data-root>/projects/<project-id>`. Its agent
history remains in `sessions/sessions.sqlite3`. The harness owns the database schema and commit
protocol, while the SDK defines domain event and replay semantics; their crates are `harness` and
`agent`, respectively. The built-in Default project uses the reserved ID `default` and the
same directory shape as every other project.

Session configuration is persisted in session metadata when a session is created or changed.
The app database therefore supplies defaults; the project session database remains the durable
record of the configuration actually used. Future project-specific defaults should be keyed by
`project_id` in the app database unless they become part of the agent runtime's canonical
session semantics.

## Layout

The default root is `~/.castle`. Before opening databases, `AppStore::default_root` renames
`~/.kcastle` to that path if only the old directory exists. The old application must be closed
first. A file lock serializes simultaneous Castle migrations, and one filesystem rename moves
the whole directory, including SQLite WAL/SHM files, without copying or rewriting databases.
Failure stops startup; both directories existing leaves both untouched and uses `~/.castle`.
Symlink or non-directory legacy roots require manual migration. `CASTLE_DATA_DIR` bypasses
this behavior and selects its path verbatim. No downgrade or directory merge is performed.

```text
<data-root>/
├── app.sqlite3
└── projects/
    ├── default/
    │   └── sessions/
    │       └── sessions.sqlite3
    └── <project-id>/
        └── sessions/
            └── sessions.sqlite3
```

Project storage paths are derived from validated stable IDs rather than persisted independently.
Workspace relocation changes the external workspace path without changing the project ID or its
session directory.

The app database, WAL, and shared-memory sidecars use mode `0600` on Unix because provider API keys
remain stored as application data. Moving credentials to the operating-system credential store is
a separate security boundary and does not change this database ownership model.
