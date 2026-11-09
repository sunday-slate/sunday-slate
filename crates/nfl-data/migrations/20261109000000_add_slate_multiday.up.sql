-- A slate may span two Eastern calendar days (e.g. a Saturday/Sunday
-- postseason weekend). Persisted so status stays a pure read.
ALTER TABLE slates ADD COLUMN multiday INTEGER NOT NULL DEFAULT 0;
