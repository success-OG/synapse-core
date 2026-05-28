-- Add trace_id column to transactions table for distributed tracing
ALTER TABLE transactions ADD COLUMN trace_id VARCHAR(32) DEFAULT NULL;
CREATE INDEX idx_transactions_trace_id ON transactions(trace_id);
