-- Fixation becomes explicit: membership rows exist for every populated week,
-- tentative ones included, so presence alone no longer means fixed.
ALTER TABLE slates ADD COLUMN fixed INTEGER NOT NULL DEFAULT 0;
