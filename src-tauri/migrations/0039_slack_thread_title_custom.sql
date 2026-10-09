-- 1 when the user renamed the thread (or the extension sent X-Devdy-Title):
-- re-exports of the same thread then keep the title instead of overwriting it.
ALTER TABLE slack_threads ADD COLUMN title_custom INTEGER NOT NULL DEFAULT 0;
