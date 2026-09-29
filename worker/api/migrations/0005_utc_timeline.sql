-- Compact UTC minute totals share the existing per-day/model row. Historical
-- collector-local rows remain intact until their collector resynchronizes.
ALTER TABLE daily_rows ADD COLUMN timeline TEXT;
ALTER TABLE daily_rows ADD COLUMN source_time_zone TEXT;
