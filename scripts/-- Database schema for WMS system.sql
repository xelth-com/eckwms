-- Database schema for WMS system

-- Enable UUID extension
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Users table
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    username VARCHAR(50) NOT NULL UNIQUE,
    password_hash VARCHAR(255) NOT NULL,
    company VARCHAR(100),
    email VARCHAR(100),
    phone VARCHAR(20),
    street VARCHAR(100),
    house_number VARCHAR(20),
    postal_code VARCHAR(20),
    city VARCHAR(100),
    country VARCHAR(100),
    role VARCHAR(20) NOT NULL DEFAULT 'user',
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Item classes (must be created before items, boxes, places, and orders)
CREATE TABLE classes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    class_name VARCHAR(50) NOT NULL UNIQUE,
    part_numbers JSONB, -- Store number and type
    description JSONB,
    properties JSONB, -- Store material, color, etc.
    relations JSONB, -- Store part_of, purpose, consist_of
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Places table
CREATE TABLE places (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    serial_number VARCHAR(19) NOT NULL UNIQUE,
    class_id UUID,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    description JSONB,
    contents JSONB, -- Store item_id/box_id, timestamp
    FOREIGN KEY (class_id) REFERENCES classes(id)
);

-- Items table
CREATE TABLE items (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    serial_number VARCHAR(19) NOT NULL UNIQUE,
    class_id UUID,
    user_id UUID,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    description JSONB,
    condition JSONB,
    actions JSONB, -- Store type, message, timestamp
    images JSONB,
    mass JSONB,
    size JSONB,
    owner JSONB,
    barcodes JSONB,
    location_history JSONB, -- Store location_id, timestamp
    current_location_id UUID,
    attributes JSONB,
    status VARCHAR(20) NOT NULL DEFAULT 'active',
    FOREIGN KEY (class_id) REFERENCES classes(id),
    FOREIGN KEY (user_id) REFERENCES users(id)
);

-- Boxes table
CREATE TABLE boxes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    serial_number VARCHAR(19) NOT NULL UNIQUE,
    class_id UUID,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    description JSONB,
    barcodes JSONB,
    mass JSONB,
    size JSONB,
    contents JSONB, -- Store item_id, timestamp
    incoming JSONB, -- Store source_id, timestamp
    outgoing JSONB, -- Store destination_id, timestamp
    multiplier INTEGER DEFAULT 1,
    location_history JSONB, -- Store location_id, timestamp
    current_location_id UUID,
    status VARCHAR(20) NOT NULL DEFAULT 'active',
    FOREIGN KEY (class_id) REFERENCES classes(id)
);

-- Orders table
CREATE TABLE orders (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    serial_number VARCHAR(19) NOT NULL UNIQUE,
    class_id UUID,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    customer_id UUID,
    company VARCHAR(100),
    person VARCHAR(100),
    street VARCHAR(100),
    house_number VARCHAR(20),
    postal_code VARCHAR(20),
    city VARCHAR(100),
    country VARCHAR(100),
    contact_email VARCHAR(100),
    invoice_email VARCHAR(100),
    phone VARCHAR(20),
    contents JSONB, -- Store box_id/item_id, timestamp
    declarations JSONB, -- Store item_id, description
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    FOREIGN KEY (class_id) REFERENCES classes(id),
    FOREIGN KEY (customer_id) REFERENCES users(id)
);

-- Translation dictionary
CREATE TABLE translations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    original_text TEXT NOT NULL,
    language_code VARCHAR(5) NOT NULL,
    translated_text TEXT NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    UNIQUE (original_text, language_code)
);

-- Serial number counters
CREATE TABLE serial_counters (
    id SERIAL PRIMARY KEY,
    counter_name VARCHAR(20) NOT NULL UNIQUE,
    current_value BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);

-- Initialize serial counters
INSERT INTO serial_counters (counter_name, current_value) VALUES
    ('item_counter', 1),
    ('box_counter', 1),
    ('place_counter', 1),
    ('order_counter', 999999999999999);

-- Audit log
CREATE TABLE audit_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID,
    action_type VARCHAR(50) NOT NULL,
    entity_type VARCHAR(20) NOT NULL,
    entity_id UUID,
    details JSONB,
    ip_address VARCHAR(45),
    user_agent TEXT,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    FOREIGN KEY (user_id) REFERENCES users(id)
);

-- Item history table
CREATE TABLE item_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    item_id UUID NOT NULL,
    event_type VARCHAR(50) NOT NULL,
    event_data JSONB,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    FOREIGN KEY (item_id) REFERENCES items(id)
);

-- Box history table
CREATE TABLE box_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    box_id UUID NOT NULL,
    event_type VARCHAR(50) NOT NULL,
    event_data JSONB,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    FOREIGN KEY (box_id) REFERENCES boxes(id)
);

-- Function to update timestamps
CREATE OR REPLACE FUNCTION update_modified_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Triggers for updated_at
CREATE TRIGGER update_users_modtime
BEFORE UPDATE ON users
FOR EACH ROW EXECUTE FUNCTION update_modified_column();

CREATE TRIGGER update_classes_modtime
BEFORE UPDATE ON classes
FOR EACH ROW EXECUTE FUNCTION update_modified_column();

CREATE TRIGGER update_translations_modtime
BEFORE UPDATE ON translations
FOR EACH ROW EXECUTE FUNCTION update_modified_column();

CREATE TRIGGER update_orders_modtime
BEFORE UPDATE ON orders
FOR EACH ROW EXECUTE FUNCTION update_modified_column();

-- Indexes for common queries
CREATE INDEX idx_items_serial_number ON items(serial_number);
CREATE INDEX idx_boxes_serial_number ON boxes(serial_number);
CREATE INDEX idx_orders_serial_number ON orders(serial_number);
CREATE INDEX idx_items_current_location ON items(current_location_id);
CREATE INDEX idx_boxes_current_location ON boxes(current_location_id);
CREATE INDEX idx_translations_lang ON translations(language_code, original_text);

-- Function to generate next serial number
CREATE OR REPLACE FUNCTION generate_serial_number(counter_name TEXT, prefix CHAR)
RETURNS TEXT AS $$
DECLARE
    next_val BIGINT;
BEGIN
    UPDATE serial_counters
    SET current_value = current_value + 1,
        updated_at = NOW()
    WHERE counter_name = $1
    RETURNING current_value INTO next_val;
    
    RETURN prefix || LPAD(next_val::TEXT, 18, '0');
END;
$$ LANGUAGE plpgsql;