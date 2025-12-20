-- =====================================================
-- Migration: Add status column to registered_devices
-- =====================================================
-- Description: Adds an ENUM status field to support device quarantine/approval workflow
-- Date: 2025-12-20

-- STEP 1: Create ENUM type for device status
DO $$ BEGIN
    CREATE TYPE device_status AS ENUM ('active', 'pending', 'blocked');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- STEP 2: Add status column to registered_devices table
ALTER TABLE registered_devices
ADD COLUMN IF NOT EXISTS status device_status DEFAULT 'pending' NOT NULL;

-- STEP 3: Set existing devices to 'active' status (backward compatibility)
-- This assumes all currently registered devices should be active
UPDATE registered_devices
SET status = 'active'
WHERE status = 'pending';

-- STEP 4: Create index for faster status queries
CREATE INDEX IF NOT EXISTS idx_registered_devices_status ON registered_devices (status);

-- STEP 5: Add comment for documentation
COMMENT ON COLUMN registered_devices.status IS 'Device registration status: active (approved), pending (quarantined), blocked (denied)';

-- =====================================================
-- Rollback instructions (manual):
--
-- To rollback this migration:
-- ALTER TABLE registered_devices DROP COLUMN IF EXISTS status;
-- DROP TYPE IF EXISTS device_status;
-- DROP INDEX IF EXISTS idx_registered_devices_status;
-- =====================================================
