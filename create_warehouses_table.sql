-- Create warehouses table
CREATE TABLE IF NOT EXISTS warehouses (
  id SERIAL PRIMARY KEY,
  name VARCHAR(100) NOT NULL,
  id_offset INTEGER NOT NULL DEFAULT 0,
  is_active BOOLEAN NOT NULL DEFAULT true,
  "createdAt" TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
  "updatedAt" TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- Create indexes
CREATE INDEX IF NOT EXISTS warehouses_is_active ON warehouses(is_active);
CREATE UNIQUE INDEX IF NOT EXISTS warehouses_id_offset ON warehouses(id_offset);

-- Insert default warehouse if not exists
INSERT INTO warehouses (name, id_offset, is_active)
VALUES ('Main Warehouse', 0, true)
ON CONFLICT (id_offset) DO NOTHING;
