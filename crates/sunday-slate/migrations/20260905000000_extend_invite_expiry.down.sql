-- Restore the 48-hour policy. Lossy by nature: the per-row timestamps this
-- replaced are gone, so every outstanding invite gets a fresh 48-hour window
-- rather than the exact expiry it had before the up migration.
UPDATE invites
SET expires_at = datetime('now', 'subsec', '+48 hours')
WHERE accepted_at IS NULL;
