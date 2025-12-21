-- Table for linking external codes (EAN, DHL) to internal IDs
CREATE TABLE IF NOT EXISTS product_aliases (
    id SERIAL PRIMARY KEY,
    external_code VARCHAR(255) NOT NULL,
    internal_id VARCHAR(255) NOT NULL,
    type VARCHAR(50) NOT NULL, -- 'ean', 'tracking', 'serial', 'manual_link'
    is_verified BOOLEAN DEFAULT FALSE, -- True if human confirmed
    confidence_score INTEGER DEFAULT 0,
    created_context VARCHAR(100), -- 'receiving', 'moving', etc.
    "createdAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(external_code, internal_id)
);

CREATE INDEX idx_aliases_external ON product_aliases(external_code);
CREATE INDEX idx_aliases_internal ON product_aliases(internal_id);
