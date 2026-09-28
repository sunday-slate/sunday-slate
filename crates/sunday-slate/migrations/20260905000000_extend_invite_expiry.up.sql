-- Invite links moved from a 48-hour to a 14-day window. Put every outstanding
-- invite on the new clock, including ones that have already lapsed — a dead
-- invite is better revived than left for the commissioner to re-send by hand.
-- Accepted invites keep their original expires_at: that token is spent, and the
-- timestamp is a record of when it would have lapsed.
UPDATE invites
SET expires_at = datetime('now', 'subsec', '+14 days')
WHERE accepted_at IS NULL;
