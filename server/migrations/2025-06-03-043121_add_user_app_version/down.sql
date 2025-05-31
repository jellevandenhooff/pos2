-- This file should undo anything in `up.sql`
ALTER TABLE user_apps DROP COLUMN version;
DROP INDEX user_apps_user_id_name;
