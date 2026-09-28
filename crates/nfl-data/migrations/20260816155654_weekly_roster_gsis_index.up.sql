-- `by_gsis` names a player the `players` table does not carry, looking him up
-- by gsis id alone with no season/week/team to narrow on. The table's
-- UNIQUE (season, week, team, gsis_id) puts gsis_id last, so it cannot serve
-- that lookup, and neither can idx_..._season_week_team.
--
-- SQLite rescues the single-equality form with a skip-scan over the ~1,200
-- distinct (season, week, team) prefixes, which measured ~3.8 ms per nine-slot
-- lineup against a 96k-row table. An IN-list form gets no such rescue and
-- degrades to a full scan.
--
-- The trailing season/week columns carry the query's ORDER BY, so the
-- LIMIT 1 reads one index row instead of sorting the matches.
CREATE INDEX idx_weekly_roster_entries_gsis
    ON weekly_roster_entries (gsis_id, season DESC, week DESC);
