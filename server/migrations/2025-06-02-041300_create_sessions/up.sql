CREATE TABLE users (
	id INTEGER PRIMARY KEY NOT NULL,
	name text NOT NULL
);

CREATE TABLE user_sessions (
	id TEXT PRIMARY KEY NOT NULL,
	user_id INTEGER NOT NULL
);

INSERT INTO users (name) VALUES ("jelle"), ("bob");
