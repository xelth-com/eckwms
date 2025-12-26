-- Migration: 008-add-rack-coordinates.sql
-- Description: Add posX and posY coordinates to warehouse_racks table for visual blueprint.

ALTER TABLE warehouse_racks 
ADD COLUMN IF NOT EXISTS "posX" INTEGER DEFAULT 0,
ADD COLUMN IF NOT EXISTS "posY" INTEGER DEFAULT 0;
