# ecKasse ↔ eckWMS Integration Architecture

## Executive Summary

**Business Model:**
- **eckWMS**: Free, open-source warehouse management system
- **ecKasse**: Premium POS add-on (~€25 all-inclusive with reseller margin ~€15 net)
- **Value Proposition**: Users who love free eckWMS can upgrade with affordable POS capabilities

---

## Integration Architecture

### Option 1: Microservice Architecture (Recommended)

```
┌─────────────────────────────────────────────────────────────┐
│                    User's Business Network                  │
│                                                             │
│  ┌──────────────┐                    ┌─────────────────┐   │
│  │   eckWMS     │◄──────REST API────►│    ecKasse      │   │
│  │  (Free)      │                    │   (Premium)     │   │
│  │              │                    │                 │   │
│  │  Port: 3100  │                    │  Port: 3001     │   │
│  │              │                    │                 │   │
│  │  - Inventory │                    │  - POS          │   │
│  │  - Scanning  │                    │  - Payments     │   │
│  │  - Warehouse │                    │  - Receipts     │   │
│  │  - Shipping  │                    │  - Cash mgmt    │   │
│  └──────┬───────┘                    └────────┬────────┘   │
│         │                                     │            │
│         │         ┌──────────────────┐        │            │
│         └────────►│  PostgreSQL DB   │◄───────┘            │
│                   │                  │                     │
│                   │  Shared Tables:  │                     │
│                   │  - products      │                     │
│                   │  - inventory     │                     │
│                   │  - transactions  │                     │
│                   └──────────────────┘                     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

#### Architecture Details

**Communication Pattern:**
- **Bidirectional REST API** between services
- **Shared PostgreSQL database** with separate schemas
- **Event-driven updates** via database triggers or message queue

**Database Schema:**

```sql
-- Shared schema for both systems
CREATE SCHEMA shared;

-- WMS-specific schema
CREATE SCHEMA wms;

-- POS-specific schema
CREATE SCHEMA pos;

-- Example shared tables
shared.products
shared.inventory_levels
shared.locations
shared.users

-- WMS-specific tables
wms.scan_sessions
wms.shipments
wms.receiving
wms.stock_movements

-- POS-specific tables
pos.sales_transactions
pos.cash_drawer
pos.receipts
pos.payment_methods
pos.fiscal_records
```

**API Endpoints:**

**eckWMS → ecKasse:**
```javascript
// When product sold via POS, update WMS inventory
POST /api/wms/inventory/adjust
{
  "product_id": "123",
  "quantity": -1,
  "reason": "pos_sale",
  "transaction_id": "sale_456",
  "location_id": "store_1"
}

// Get current inventory levels
GET /api/wms/inventory/levels?product_ids=123,456,789
```

**ecKasse → eckWMS:**
```javascript
// When product received in warehouse, notify POS
POST /api/pos/inventory/notify
{
  "product_id": "123",
  "quantity_change": +50,
  "location": "warehouse",
  "timestamp": "2025-12-21T10:00:00Z"
}

// Get product information for POS
GET /api/pos/products?barcode=1234567890
```

**Authentication:**
- Shared JWT tokens between services
- Common `users` table with role-based access control
- API keys for service-to-service communication

---

### Option 2: Monolithic with Module Isolation

```
┌───────────────────────────────────────────────┐
│         eckWMS + ecKasse (Unified)           │
│                                               │
│  ┌──────────────┐      ┌─────────────────┐   │
│  │  WMS Module  │      │   POS Module    │   │
│  │   (Free)     │      │   (Licensed)    │   │
│  │              │      │                 │   │
│  │  /wms/*      │      │   /pos/*        │   │
│  └──────┬───────┘      └────────┬────────┘   │
│         │                       │            │
│         └───────┬───────────────┘            │
│                 │                            │
│         ┌───────▼────────┐                   │
│         │  Shared Core   │                   │
│         │  - Auth        │                   │
│         │  - Database    │                   │
│         │  - API         │                   │
│         └────────────────┘                   │
│                                               │
└───────────────────────────────────────────────┘
```

**Pros:**
- Simpler deployment (single service)
- Shared code and utilities
- Easier transaction management

**Cons:**
- Harder to sell separately
- User must install POS even if not using it
- Less scalable

---

### Option 3: Plugin/Extension Model (Best for Free→Paid)

```
┌─────────────────────────────────────────────┐
│            eckWMS (Core - Free)            │
│                                             │
│  ┌──────────────────────────────┐          │
│  │     Plugin Manager           │          │
│  │                              │          │
│  │  Registered Plugins:         │          │
│  │  ✓ core-inventory (free)     │          │
│  │  ✓ core-scanning (free)      │          │
│  │  ✓ ecKasse-POS (€25) ◄───────┼──────┐   │
│  │                              │      │   │
│  └──────────────────────────────┘      │   │
│                                         │   │
│  ┌──────────────────────────────┐      │   │
│  │    License Validator         │      │   │
│  │  - Check activation key      │      │   │
│  │  - Verify domain/MAC         │      │   │
│  │  - Enable/disable features   │      │   │
│  └──────────────────────────────┘      │   │
│                                         │   │
└─────────────────────────────────────────┼───┘
                                          │
                      ┌───────────────────▼───────┐
                      │   ecKasse Plugin Package  │
                      │   (npm package)           │
                      │   - POS routes            │
                      │   - POS controllers       │
                      │   - POS frontend          │
                      │   - License: Commercial   │
                      └───────────────────────────┘
```

**Implementation:**

```javascript
// eckWMS plugin system
// src/server/plugins/pluginManager.js

class PluginManager {
  async loadPlugin(pluginName, licenseKey) {
    // Verify license
    const isValid = await this.verifyLicense(pluginName, licenseKey);

    if (!isValid) {
      throw new Error('Invalid license');
    }

    // Load plugin dynamically
    const plugin = require(pluginName);

    // Register routes
    this.app.use(plugin.routes);

    // Register database migrations
    await plugin.migrate(this.db);

    // Enable features
    this.enableFeatures(plugin.features);
  }

  async verifyLicense(pluginName, licenseKey) {
    // Call licensing server
    const response = await fetch('https://pda.repair/api/license/verify', {
      method: 'POST',
      body: JSON.stringify({
        plugin: pluginName,
        key: licenseKey,
        instance_id: process.env.INSTANCE_ID
      })
    });

    return response.ok;
  }
}
```

**License Activation Flow:**

1. User installs free eckWMS
2. User purchases ecKasse license (€25)
3. User receives activation key
4. User enters key in eckWMS settings
5. eckWMS validates key with licensing server
6. ecKasse plugin automatically downloads and activates
7. POS features appear in menu

**Licensing Server Requirements:**
- License generation and validation API
- Instance tracking (prevent key sharing)
- Automatic updates for licensed plugins
- Trial period support (14 days)

---

## Recommended Approach: Hybrid (Option 1 + Option 3)

**Best of both worlds:**

1. **Microservices for scalability**: ecKasse runs as separate service
2. **Plugin registration**: eckWMS "knows" about ecKasse via plugin manager
3. **Shared database**: Both use same PostgreSQL with different schemas
4. **License control**: Plugin manager controls activation
5. **Flexible deployment**:
   - Small business: Both on same machine
   - Growing business: Separate servers

```javascript
// Example: eckWMS discovers ecKasse
// config/plugins.json
{
  "installed_plugins": [
    {
      "name": "ecKasse-POS",
      "type": "microservice",
      "url": "http://localhost:3001",
      "license_key": "XXXX-XXXX-XXXX-XXXX",
      "status": "active",
      "features": [
        "pos_sales",
        "cash_drawer",
        "fiscal_receipts",
        "payment_processing"
      ]
    }
  ]
}
```

---

## Revenue Model

**Pricing Tiers:**

| Tier | Price | Features | Support |
|------|-------|----------|---------|
| eckWMS Free | €0 | Full WMS, unlimited items | Community |
| ecKasse Basic | €15 | + POS, 1 register | Email |
| ecKasse Plus | €25 | + Multiple registers, fiscal | Priority |
| ecKasse Pro | €50 | + Multi-location, reports | Phone/Chat |

**Reseller Program:**
- Reseller buys at 40% discount (€15 → €9)
- Sells at recommended €25 or their own price
- Margin: €16 per license
- Volume discounts: 10+ licenses = 50% discount

**Recurring Revenue Options:**
1. Annual license renewal (optional)
2. Cloud hosting service (€5/month)
3. Premium support plans (€10/month)
4. Custom integrations (project-based)

---

## Technical Integration Points

### Shared Services (Both Systems)

1. **Authentication**
   - JWT tokens
   - User management
   - Role-based access

2. **Database**
   - Product catalog
   - Inventory levels
   - Transaction history

3. **AI Features (Gemini)**
   - Product search
   - Smart categorization
   - Report generation

### eckWMS-Specific

- Barcode scanning
- Warehouse locations
- Receiving/shipping
- Stock movements
- Google Sheets export

### ecKasse-Specific

- POS interface
- Payment processing
- Receipt printing
- Cash drawer management
- Fiscal compliance (Germany)
- DSFinV-K export

---

## Data Flow Example

**Scenario: Customer buys product at POS**

```
1. ecKasse: Scan product → Product DB lookup
2. ecKasse: Create sale transaction
3. ecKasse: Process payment
4. ecKasse: Print receipt
5. ecKasse → eckWMS API: POST /api/inventory/adjust
   {
     product_id: "123",
     quantity: -1,
     source: "pos_sale",
     transaction_id: "SALE_20251221_001"
   }
6. eckWMS: Update inventory level
7. eckWMS: Log stock movement
8. eckWMS: Trigger reorder if below threshold
9. (Optional) eckWMS → Google Sheets: Export sale data
```

---

## Implementation Roadmap

### Phase 1: Foundation (Week 1-2)
- ✅ Setup Gemini services in eckWMS
- ✅ Copy proven architecture from ecKasse
- ⏳ Create plugin manager in eckWMS
- ⏳ Design shared database schema

### Phase 2: Integration (Week 3-4)
- Create REST API endpoints
- Implement authentication sharing
- Build inventory sync mechanism
- Test data flow

### Phase 3: Licensing (Week 5-6)
- Build license server
- Implement activation flow
- Create payment integration
- Setup reseller portal

### Phase 4: Launch (Week 7-8)
- Documentation
- Marketing materials
- Reseller onboarding
- Beta testing

---

## Security Considerations

1. **License Protection**
   - Hardware fingerprinting (MAC address)
   - Online activation required
   - Periodic validation checks
   - Encrypted license keys

2. **Data Security**
   - API authentication between services
   - Database-level access control
   - Audit logging for all sales
   - GDPR compliance

3. **Deployment Security**
   - HTTPS for all communication
   - Firewall rules between services
   - Regular security updates
   - Vulnerability scanning

---

## Support & Maintenance

**Free eckWMS:**
- Community forum
- GitHub issues
- Basic documentation

**Paid ecKasse:**
- Email support (24h response)
- Video tutorials
- Priority bug fixes
- Custom feature requests (Pro tier)

---

## Future Extensions

**Potential Additional Paid Modules:**

1. **ecAnalytics** (€10/month)
   - Advanced reporting
   - Business intelligence
   - Forecasting

2. **ecMulti** (€30/month)
   - Multi-warehouse support
   - Franchise management
   - Centralized control

3. **ecCommerce** (€20/month)
   - WooCommerce integration
   - Shopify sync
   - Amazon FBA

4. **ecMobile** (€15/month)
   - Native mobile apps
   - Offline mode
   - Advanced barcode scanning

---

## Conclusion

**Recommended Implementation:**
- Start with Plugin Model (Option 3)
- Architecture ready for Microservices (Option 1)
- Launch with €25 all-inclusive pricing
- Focus on easy activation and smooth UX
- Build trust with free tier, monetize with premium POS

**Next Steps:**
1. Finalize shared database schema
2. Build license validation service
3. Create ecKasse npm package structure
4. Implement plugin manager in eckWMS
5. Setup pda.repair licensing server

---

*Last Updated: 2025-12-21*
*Author: System Architect*
