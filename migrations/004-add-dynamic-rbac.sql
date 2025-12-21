-- =====================================================
-- Migration: Dynamic RBAC (Role-Based Access Control)
-- =====================================================

-- 1. Create Permissions Table (Atomic capabilities)
CREATE TABLE IF NOT EXISTS permissions (
    id SERIAL PRIMARY KEY,
    slug VARCHAR(100) NOT NULL UNIQUE, -- e.g., 'scan.execute', 'settings.edit'
    description TEXT
);

-- 2. Create Roles Table
CREATE TABLE IF NOT EXISTS roles (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL UNIQUE,
    description TEXT,
    is_system_protected BOOLEAN DEFAULT FALSE, -- If TRUE, Agent cannot modify this role
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 3. Create Role-Permissions Link Table
CREATE TABLE IF NOT EXISTS role_permissions (
    role_id INTEGER REFERENCES roles(id) ON DELETE CASCADE,
    permission_id INTEGER REFERENCES permissions(id) ON DELETE CASCADE,
    PRIMARY KEY (role_id, permission_id)
);

-- 4. Update Registered Devices to use Roles
ALTER TABLE registered_devices ADD COLUMN IF NOT EXISTS role_id INTEGER REFERENCES roles(id) ON DELETE SET NULL;

-- =====================================================
-- SEED DATA: Standard WMS Roles & Permissions
-- =====================================================

-- A. Define Permissions
INSERT INTO permissions (slug, description) VALUES
    ('core.admin', 'Full system access (Red Button)'),
    ('settings.view', 'View device settings'),
    ('settings.edit', 'Edit device settings'),
    ('scan.execute', 'Perform barcode scans'),
    ('workflow.receiving', 'Execute receiving workflows'),
    ('workflow.picking', 'Execute picking workflows'),
    ('workflow.packing', 'Execute packing workflows'),
    ('workflow.inventory', 'Execute inventory/cycle count workflows'),
    ('inventory.adjust', 'Make stock adjustments')
ON CONFLICT (slug) DO NOTHING;

-- B. Define Roles
-- 1. SUPER ADMIN (Protected)
INSERT INTO roles (name, description, is_system_protected)
VALUES ('SUPER_ADMIN', 'System Root - The Red Button', TRUE)
ON CONFLICT (name) DO NOTHING;

-- 2. WAREHOUSE MANAGER
INSERT INTO roles (name, description)
VALUES ('MANAGER', 'Warehouse Manager - Can oversee operations')
ON CONFLICT (name) DO NOTHING;

-- 3. INBOUND CLERK
INSERT INTO roles (name, description)
VALUES ('INBOUND', 'Receiver - Handles incoming shipments')
ON CONFLICT (name) DO NOTHING;

-- 4. PICKER
INSERT INTO roles (name, description)
VALUES ('PICKER', 'Picker - Collects items for orders')
ON CONFLICT (name) DO NOTHING;

-- C. Assign Permissions to Roles (Basic Mapping)
-- Super Admin gets everything
INSERT INTO role_permissions (role_id, permission_id)
SELECT (SELECT id FROM roles WHERE name = 'SUPER_ADMIN'), id FROM permissions
ON CONFLICT DO NOTHING;

-- Manager gets all except core.admin
INSERT INTO role_permissions (role_id, permission_id)
SELECT (SELECT id FROM roles WHERE name = 'MANAGER'), id FROM permissions WHERE slug != 'core.admin'
ON CONFLICT DO NOTHING;

-- Inbound gets scanning and receiving
INSERT INTO role_permissions (role_id, permission_id)
SELECT (SELECT id FROM roles WHERE name = 'INBOUND'), id FROM permissions
WHERE slug IN ('scan.execute', 'workflow.receiving', 'settings.view')
ON CONFLICT DO NOTHING;

-- Picker gets scanning and picking
INSERT INTO role_permissions (role_id, permission_id)
SELECT (SELECT id FROM roles WHERE name = 'PICKER'), id FROM permissions
WHERE slug IN ('scan.execute', 'workflow.picking', 'settings.view')
ON CONFLICT DO NOTHING;
