-- Migration 009: Multi-warehouse support with configurable ID offsets
-- This enables managing multiple physical warehouses with distinct ID ranges

-- Create warehouses table
CREATE TABLE IF NOT EXISTS warehouses (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    id_offset BIGINT NOT NULL DEFAULT 0,
    is_active BOOLEAN DEFAULT TRUE,
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- Insert default warehouse (Main Warehouse, offset 0)
INSERT INTO warehouses (name, id_offset, is_active)
VALUES ('Main Warehouse', 0, TRUE)
ON CONFLICT DO NOTHING;

-- Add warehouse reference to racks table
ALTER TABLE warehouse_racks
ADD COLUMN IF NOT EXISTS warehouse_id INTEGER REFERENCES warehouses(id) ON DELETE CASCADE;

-- Add visual sizing columns for custom rack display
ALTER TABLE warehouse_racks
ADD COLUMN IF NOT EXISTS visual_width INTEGER DEFAULT 0;

ALTER TABLE warehouse_racks
ADD COLUMN IF NOT EXISTS visual_height INTEGER DEFAULT 0;

-- Migrate existing racks to the default warehouse
UPDATE warehouse_racks
SET warehouse_id = (SELECT id FROM warehouses ORDER BY id ASC LIMIT 1)
WHERE warehouse_id IS NULL;

-- Add index for performance
CREATE INDEX IF NOT EXISTS idx_warehouse_racks_warehouse_id ON warehouse_racks(warehouse_id);

-- Comment for documentation
COMMENT ON TABLE warehouses IS 'Stores multiple warehouse configurations with ID offsets for inventory segregation';
COMMENT ON COLUMN warehouses.id_offset IS 'Starting ID offset for this warehouse (e.g., 0 for WH1, 10000000 for WH2)';
COMMENT ON COLUMN warehouse_racks.visual_width IS 'Custom visual width override in pixels (0 = auto-calculate)';
COMMENT ON COLUMN warehouse_racks.visual_height IS 'Custom visual height override in pixels (0 = auto-calculate)';
