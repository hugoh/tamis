/// `MIGRATIONS[i]` upgrades the schema from version `i` to `i + 1`. Released
/// steps never change; new ones are appended. They match the Go version's, so
/// its databases open unchanged.
pub(super) const MIGRATIONS: &[&str] = &[
    "
CREATE TABLE tokens (
	token TEXT PRIMARY KEY,
	spam  INTEGER NOT NULL DEFAULT 0,
	good  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE trained (
	key    TEXT PRIMARY KEY,
	label  INTEGER NOT NULL,
	tokens TEXT NOT NULL
);
CREATE TABLE addresses (
	address  TEXT PRIMARY KEY,
	source   TEXT NOT NULL,
	added_at INTEGER NOT NULL
);
CREATE TABLE cursors (
	mailbox      TEXT PRIMARY KEY,
	uid_validity INTEGER NOT NULL,
	last_uid     INTEGER NOT NULL
);
CREATE TABLE decisions (
	id         INTEGER PRIMARY KEY,
	at         INTEGER NOT NULL,
	mailbox    TEXT NOT NULL,
	uid        INTEGER NOT NULL,
	message_id TEXT NOT NULL,
	sender     TEXT NOT NULL,
	subject    TEXT NOT NULL,
	score      REAL NOT NULL,
	verdict    TEXT NOT NULL,
	reason     TEXT NOT NULL,
	action     TEXT NOT NULL,
	applied    INTEGER NOT NULL
);
",
    "ALTER TABLE decisions ADD COLUMN undone INTEGER NOT NULL DEFAULT 0;",
    "CREATE INDEX decisions_by_message_id ON decisions (message_id);",
    "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    "CREATE TABLE mod_seqs (
	mailbox      TEXT PRIMARY KEY,
	uid_validity INTEGER NOT NULL,
	highest      INTEGER NOT NULL
);",
    "CREATE TABLE passkeys (
	id            INTEGER PRIMARY KEY,
	credential_id TEXT NOT NULL UNIQUE,
	name          TEXT NOT NULL,
	credential    BLOB NOT NULL,
	created_at    INTEGER NOT NULL,
	last_used     INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE enrollments (
	token_hash TEXT PRIMARY KEY,
	expires_at INTEGER NOT NULL
);",
    "CREATE INDEX decisions_by_at ON decisions (at, id);",
    "CREATE TABLE pending_moves (
	decision_id INTEGER PRIMARY KEY,
	source      TEXT NOT NULL,
	dest        TEXT NOT NULL,
	message_id  TEXT NOT NULL,
	keyword     TEXT NOT NULL
);",
    "ALTER TABLE pending_moves ADD COLUMN restore TEXT NOT NULL DEFAULT '';
ALTER TABLE pending_moves ADD COLUMN undoes INTEGER NOT NULL DEFAULT 0;",
];
