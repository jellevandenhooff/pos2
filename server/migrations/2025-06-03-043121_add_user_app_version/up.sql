-- Your SQL goes here
ALTER TABLE user_apps ADD COLUMN version TEXT;
UPDATE user_apps SET version = '';
CREATE UNIQUE INDEX user_apps_user_id_name ON user_apps (user_id, name);
