ALTER TABLE objects
ADD COLUMN is_noise INTEGER NOT NULL DEFAULT 0;

UPDATE objects
SET is_noise = 1
WHERE title IN (
    '.gitignore',
    '.gitattributes',
    '.gitmodules',
    '.editorconfig',
    '.DS_Store',
    'Thumbs.db',
    'Cargo.lock',
    'package-lock.json',
    'pnpm-lock.yaml',
    'yarn.lock',
    'bun.lockb',
    'poetry.lock',
    'uv.lock',
    'flake.lock'
);

CREATE INDEX IF NOT EXISTS idx_objects_noise ON objects(is_noise);
CREATE INDEX IF NOT EXISTS idx_objects_project_noise ON objects(project_id, is_deleted, is_noise);
