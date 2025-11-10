-- =====================================================
-- Fix: Create tables in correct order
-- =====================================================

-- STEP 1: Create eckwms_instances first
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

-- Insert InBody instance
INSERT INTO eckwms_instances (id, name, server_url, api_key, tier)
VALUES (
    '00000000-0000-0000-0000-000000000001'::uuid,
    'InBody Service Center',
    'http://192.168.11.119:3000',
    'inbody_internal_key_' || md5(random()::text),
    'paid'
)
ON CONFLICT (name) DO NOTHING;

-- STEP 2: Create scans table with proper foreign key
CREATE TABLE IF NOT EXISTS scans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payload TEXT DEFAULT '{}',
    checksum VARCHAR(255),
    "deviceId" VARCHAR(255),
    instance_id UUID REFERENCES eckwms_instances(id) ON DELETE SET NULL,
    type VARCHAR(255),
    priority INTEGER DEFAULT 0,
    status VARCHAR(255) DEFAULT 'buffered',
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- Create indexes
CREATE INDEX IF NOT EXISTS idx_scans_created_at ON scans ("createdAt" DESC);
CREATE INDEX IF NOT EXISTS idx_scans_status ON scans (status);
CREATE INDEX IF NOT EXISTS idx_scans_instance_id ON scans (instance_id);
CREATE INDEX IF NOT EXISTS idx_scans_device_id ON scans ("deviceId");
CREATE INDEX IF NOT EXISTS idx_scans_instance_status ON scans (instance_id, status);

COMMENT ON TABLE scans IS 'Unified buffer table for scans from eckwms mobile devices';

-- STEP 3: Create registered_devices table
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

-- STEP 4: Restore data from backup if exists
DO $$
DECLARE
    backup_count INTEGER;
BEGIN
    SELECT COUNT(*) INTO backup_count FROM scans_backup;

    IF backup_count > 0 THEN
        RAISE NOTICE 'Restoring % scans from backup...', backup_count;

        -- Insert data with UUID conversion
        INSERT INTO scans (id, payload, checksum, "deviceId", type, priority, status, "createdAt", "updatedAt")
        SELECT
            gen_random_uuid() as id,
            COALESCE(payload::text, '{}') as payload,
            checksum,
            "deviceId",
            type,
            COALESCE(priority, 0) as priority,
            COALESCE(status, 'buffered') as status,
            COALESCE("createdAt", NOW()) as "createdAt",
            COALESCE("updatedAt", NOW()) as "updatedAt"
        FROM scans_backup
        ON CONFLICT DO NOTHING;

        RAISE NOTICE 'Backup restored successfully';
    END IF;
END $$;

-- STEP 5: Add scan_id to repair_orders
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

CREATE INDEX IF NOT EXISTS idx_repair_orders_scan_id ON repair_orders(scan_id);
COMMENT ON COLUMN repair_orders.scan_id IS 'Link to scan that created or is associated with this repair order';

-- STEP 6: Create view
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

-- STEP 7: Create helper functions
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
    SELECT ro.id, ro.order_number, ro.customer_name, ro.device_model, ro.repair_status
    FROM repair_orders ro
    WHERE ro.scan_id = p_scan_id;
END;
$$ LANGUAGE plpgsql;

-- STEP 8: Create trigger
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

-- STEP 9: Verify and report
DO $$
DECLARE
    scans_count INTEGER;
    repair_orders_count INTEGER;
BEGIN
    SELECT COUNT(*) INTO scans_count FROM scans;
    SELECT COUNT(*) INTO repair_orders_count FROM repair_orders;

    RAISE NOTICE '==============================================';
    RAISE NOTICE 'Migration completed successfully!';
    RAISE NOTICE '==============================================';
    RAISE NOTICE 'Tables created:';
    RAISE NOTICE '  - eckwms_instances';
    RAISE NOTICE '  - scans (% records)', scans_count;
    RAISE NOTICE '  - registered_devices';
    RAISE NOTICE '  - repair_orders.scan_id (% orders)', repair_orders_count;
    RAISE NOTICE '';
    RAISE NOTICE 'Views: v_scans_with_repairs';
    RAISE NOTICE 'Functions: link_scan_to_repair_order, get_repair_order_from_scan';
    RAISE NOTICE '==============================================';
END $$;
