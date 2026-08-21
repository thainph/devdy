-- Manual display order for the project list. `position` is ascending (lower =
-- higher / earlier); drag-and-drop on the projects screen rewrites positions.
-- Existing projects are seeded from the previous usage-based ordering (run
-- frequency, recency, then name) so the list looks unchanged until reordered.
ALTER TABLE projects ADD COLUMN position INTEGER NOT NULL DEFAULT 0;

WITH ordered AS (
    SELECT p.id AS pid,
           ROW_NUMBER() OVER (
               ORDER BY COUNT(r.id) DESC, MAX(r.created_at) DESC, p.name COLLATE NOCASE ASC
           ) AS rn
    FROM projects p
    LEFT JOIN runs r ON r.project_id = p.id
    GROUP BY p.id
)
UPDATE projects
SET position = (SELECT rn FROM ordered WHERE ordered.pid = projects.id)
WHERE id IN (SELECT pid FROM ordered);

CREATE INDEX IF NOT EXISTS idx_projects_position ON projects(position);
