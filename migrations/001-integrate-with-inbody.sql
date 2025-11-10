-- =====================================================
-- eckWMS Integration with InBody Service Center
-- Migration: Sync scans table and add repair_orders link
-- =====================================================

-- =====================================================
-- PART 1: Ensure scans table has correct structure
-- =====================================================

-- Check if scans table exists with old structure (SERIAL id)
-- If yes, we'll need to migrate it to UUID structure

DO $$
BEGIN
    -- Check if scans table exists
    IF EXISTS (SELECT 1 FROM information_schema.tables
               WHERE table_name = 'scans') THEN

        -- Check if id column is already UUID
        IF EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_name = 'scans'
                   AND column_name = 'id'
                   AND data_type = 'uuid') THEN
            RAISE NOTICE 'scans table already uses UUID, skipping migration';
        ELSE
            RAISE NOTICE 'Migrating scans table from SERIAL to UUID...';

            -- Backup old scans table
            CREATE TABLE IF NOT EXISTS scans_backup AS SELECT * FROM scans;

            -- Drop old table
            DROP TABLE IF EXISTS scans CASCADE;

            RAISE NOTICE 'Old scans table backed up and dropped';
        END IF;
    END IF;
END $$;

-- =====================================================
-- Create or update scans table with eckWMS structure
-- =====================================================

CREATE TABLE IF NOT EXISTS scans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Payload and verification
    payload TEXT DEFAULT '{}',
    checksum VARCHAR(255),

    -- Device and instance identification
    "deviceId" VARCHAR(255),
    instance_id UUID REFERENCES eckwms_instances(id) ON DELETE SET NULL,

    -- Scan metadata
    type VARCHAR(255),
    priority INTEGER DEFAULT 0,
    status VARCHAR(255) DEFAULT 'buffered',

    -- Timestamps
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- Create indexes for performance
CREATE INDEX IF NOT EXISTS idx_scans_created_at ON scans ("createdAt" DESC);
CREATE INDEX IF NOT EXISTS idx_scans_status ON scans (status);
CREATE INDEX IF NOT EXISTS idx_scans_instance_id ON scans (instance_id);
CREATE INDEX IF NOT EXISTS idx_scans_device_id ON scans ("deviceId");
CREATE INDEX IF NOT EXISTS idx_scans_instance_status ON scans (instance_id, status);

-- Add comments
COMMENT ON TABLE scans IS 'Unified buffer table for scans from eckwms mobile devices, used by both WMS and InBody repair orders';
COMMENT ON COLUMN scans.instance_id IS 'Reference to eckWMS instance (NULL for standalone/InBody use)';
COMMENT ON COLUMN scans.status IS 'Scan status: buffered, delivered, confirmed, or processed';

-- =====================================================
-- PART 2: Ensure eckwms_instances table exists
-- =====================================================

CREATE TABLE IF NOT EXISTS eckwms_instances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL UNIQUE,
    server_url VARCHAR(255) NOT NULL,
    api_key VARCHAR(255) NOT NULL UNIQUE,
    tier VARCHAR(50) DEFAULT 'free' CHECK (tier IN ('free', 'paid')),
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE eckwms_instances IS 'eckWMS client instances for multi-tenant support';

-- =====================================================
-- PART 3: Ensure registered_devices table exists
-- =====================================================

CREATE TABLE IF NOT EXISTS registered_devices (
    "deviceId" VARCHAR(255) PRIMARY KEY,
    instance_id UUID REFERENCES eckwms_instances(id) ON DELETE SET NULL,
    is_active BOOLEAN DEFAULT true,
    "publicKey" TEXT NOT NULL,
    "deviceName" VARCHAR(255),
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE registered_devices IS 'Registered mobile devices for eckWMS';
COMMENT ON COLUMN registered_devices."publicKey" IS 'Base64-encoded Ed25519 public key for device authentication';

-- =====================================================
-- PART 4: Add scan_id to repair_orders (InBody Driver)
-- =====================================================

-- Add scan_id column if it doesn't exist
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_name = 'repair_orders'
                   AND column_name = 'scan_id') THEN
        ALTER TABLE repair_orders ADD COLUMN scan_id UUID REFERENCES scans(id) ON DELETE SET NULL;
        RAISE NOTICE 'Added scan_id column to repair_orders';
    ELSE
        RAISE NOTICE 'scan_id column already exists in repair_orders';
    END IF;
END $$;

-- Create index for scan_id lookups
CREATE INDEX IF NOT EXISTS idx_repair_orders_scan_id ON repair_orders(scan_id);

COMMENT ON COLUMN repair_orders.scan_id IS 'Link to scan that created or is associated with this repair order';

-- =====================================================
-- PART 5: Create InBody-specific instance (optional)
-- =====================================================

-- Insert InBody standalone instance if it doesn't exist
INSERT INTO eckwms_instances (id, name, server_url, api_key, tier)
VALUES (
    '00000000-0000-0000-0000-000000000001'::uuid,
    'InBody Service Center',
    'http://192.168.11.119:3000',
    'inbody_internal_key_' || md5(random()::text),
    'paid'
)
ON CONFLICT (name) DO NOTHING;

-- =====================================================
-- PART 6: Create views for integration
-- =====================================================

-- View: Recent scans with repair order info
CREATE OR REPLACE VIEW v_scans_with_repairs AS
SELECT
    s.id as scan_id,
    s."deviceId",
    s.payload,
    s.type,
    s.status as scan_status,
    s."createdAt" as scan_created_at,
    ro.id as repair_order_id,
    ro.order_number,
    ro.customer_name,
    ro.device_model,
    ro.device_serial,
    ro.repair_status,
    ro.error_description,
    ei.name as instance_name
FROM scans s
LEFT JOIN repair_orders ro ON ro.scan_id = s.id
LEFT JOIN eckwms_instances ei ON ei.id = s.instance_id
ORDER BY s."createdAt" DESC;

COMMENT ON VIEW v_scans_with_repairs IS 'Unified view showing scans and their associated repair orders';

-- =====================================================
-- PART 7: Helper functions
-- =====================================================

-- Function to link scan to repair order
CREATE OR REPLACE FUNCTION link_scan_to_repair_order(
    p_scan_id UUID,
    p_repair_order_id INTEGER
)
RETURNS BOOLEAN AS $$
BEGIN
    UPDATE repair_orders
    SET scan_id = p_scan_id,
        updated_at = NOW()
    WHERE id = p_repair_order_id;

    RETURN FOUND;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION link_scan_to_repair_order IS 'Links a scan to a repair order';

-- Function to get repair order from scan
CREATE OR REPLACE FUNCTION get_repair_order_from_scan(p_scan_id UUID)
RETURNS TABLE (
    order_id INTEGER,
    order_number VARCHAR(50),
    customer_name TEXT,
    device_model VARCHAR(50),
    repair_status VARCHAR(20)
) AS $$
BEGIN
    RETURN QUERY
    SELECT
        ro.id,
        ro.order_number,
        ro.customer_name,
        ro.device_model,
        ro.repair_status
    FROM repair_orders ro
    WHERE ro.scan_id = p_scan_id;
END;
$$ LANGUAGE plpgsql;

-- =====================================================
-- PART 8: Update triggers
-- =====================================================

-- Trigger to auto-update updatedAt on scans
CREATE OR REPLACE FUNCTION update_scans_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW."updatedAt" = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trigger_update_scans_timestamp ON scans;
CREATE TRIGGER trigger_update_scans_timestamp
    BEFORE UPDATE ON scans
    FOR EACH ROW
    EXECUTE FUNCTION update_scans_updated_at();

-- =====================================================
-- Migration Complete
-- =====================================================

DO $$
BEGIN
    RAISE NOTICE '==============================================';
    RAISE NOTICE 'eckWMS + InBody Integration Migration Complete';
    RAISE NOTICE '==============================================';
    RAISE NOTICE 'Tables created/updated:';
    RAISE NOTICE '  - scans (UUID-based, unified structure)';
    RAISE NOTICE '  - eckwms_instances';
    RAISE NOTICE '  - registered_devices';
    RAISE NOTICE '  - repair_orders (added scan_id column)';
    RAISE NOTICE '';
    RAISE NOTICE 'Views created:';
    RAISE NOTICE '  - v_scans_with_repairs';
    RAISE NOTICE '';
    RAISE NOTICE 'Next steps:';
    RAISE NOTICE '  1. Test connection: npm run dev:local';
    RAISE NOTICE '  2. Verify tables: SELECT * FROM v_scans_with_repairs LIMIT 5;';
    RAISE NOTICE '==============================================';
END $$;
