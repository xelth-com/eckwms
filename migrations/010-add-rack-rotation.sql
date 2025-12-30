-- Migration: Add rotation column to warehouse_racks table
-- Date: 2025-12-29
-- Purpose: Support rack rotation (0°, 90°, 180°, 270°) in blueprint view

ALTER TABLE warehouse_racks
ADD COLUMN IF NOT EXISTS rotation INTEGER DEFAULT 0;

-- Update existing records to have 0 rotation
UPDATE warehouse_racks SET rotation = 0 WHERE rotation IS NULL;
