-- Web page captures were removed. Slack threads left `captures` in 0041 (rows
-- carried over into slack_threads / slack_files, which hold no reference to
-- these tables), so the generic captures core is dropped. Indexes go with
-- their tables; files under <app_data>/web-pages/ are removed by the app.
DROP TABLE IF EXISTS capture_attachments;
DROP TABLE IF EXISTS captures;
