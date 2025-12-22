-- Table for storing global system settings and counters
CREATE TABLE IF NOT EXISTS system_settings (
    id SERIAL PRIMARY KEY,
    key VARCHAR(100) UNIQUE NOT NULL,
    value TEXT NOT NULL,
    description TEXT,
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- Create index for faster key lookups
CREATE INDEX idx_system_settings_key ON system_settings(key);

-- Insert default counter values
INSERT INTO system_settings (key, value, description) VALUES
    ('last_serial_item', '0', 'Last used serial number for items (i-prefix)'),
    ('last_serial_box', '0', 'Last used serial number for boxes (b-prefix)'),
    ('last_serial_place', '0', 'Last used serial number for places (p-prefix)'),
    ('last_serial_marker', '0', 'Last used serial number for simple markers (LPN codes)')
ON CONFLICT (key) DO NOTHING;
