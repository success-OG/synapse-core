-- Rollback: Remove trace_id column from transactions table
DROP INDEX IF EXISTS idx_transactions_trace_id;
ALTER TABLE transactions DROP COLUMN IF EXISTS trace_id;
