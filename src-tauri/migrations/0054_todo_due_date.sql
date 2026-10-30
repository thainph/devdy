-- Optional deadline for a todo: a local calendar date `YYYY-MM-DD` (no time,
-- no timezone — "due on that day" wherever the user is). NULL = no deadline.
ALTER TABLE todos ADD COLUMN due_date TEXT;
