-- Migration: 007-add-warehouse-racks.sql
-- Description: Create warehouse_racks table for structured storage management.

CREATE TABLE IF NOT EXISTS warehouse_racks (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    prefix VARCHAR(10),
    columns INTEGER NOT NULL DEFAULT 1,
    rows INTEGER NOT NULL DEFAULT 1,
    start_index INTEGER NOT NULL,
    sort_order INTEGER DEFAULT 0,
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- Index for fast sorting by order
CREATE INDEX IF NOT EXISTS idx_racks_sort ON warehouse_racks(sort_order);

-- Update timestamp trigger (if needed, but simple Sequelize/Raw update works too)
