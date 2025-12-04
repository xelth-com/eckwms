# eckWMS Global Server

Standalone microservice for eckWMS - handles instance discovery, public QR codes, and API proxying.

## Features

- ✅ Truly independent microservice (no parent directory dependencies)
- ✅ Instance registration and discovery
- ✅ Public QR code pages
- ✅ Local instance API proxying
- ✅ Health checks and monitoring
- ✅ PM2 ready
- ✅ PostgreSQL database support (optional)

## Quick Start

### Using PM2 (Recommended for Production)

```bash
# Install dependencies
npm install

# Copy environment configuration
cp .env.example .env
# Edit .env with your values

# Start with PM2 (from root directory)
cd /var/www/pda.repair
pm2 start ecosystem.config.js

# Check status
pm2 list

# View logs
pm2 logs eckwms-global
```

See [PM2_SETUP.md](./PM2_SETUP.md) for detailed PM2 setup guide.

Server will be available at: `http://localhost:8080`

### Local Development

```bash
# Install dependencies
npm install

# Copy environment configuration
cp .env.example .env

# Start development server (with auto-reload)
npm run dev

# Or start production server directly
npm start
```


## Environment Variables

See `.env.example` for all available options. Key variables:

| Variable | Default | Purpose |
|----------|---------|---------|
| `PORT` | 8080 | Server port |
| `GLOBAL_SERVER_API_KEY` | (required) | API key for internal endpoints |
| `PG_HOST` | localhost | PostgreSQL host |
| `PG_DATABASE` | eckwms_global | Database name |
| `ENC_KEY` | (required) | AES encryption key for QR codes |

## API Endpoints

### Public Endpoints

- `GET /` - API documentation
- `GET /ECK/health` - Health check (no auth required)
- `GET /ECK/:code` - Public QR code page

### Protected Endpoints (require `X-Internal-Api-Key` header)

- `POST /ECK/api/internal/register-instance` - Register new instance
- `GET /ECK/api/internal/get-instance-info/:id` - Get instance connection info
- `POST /ECK/api/internal/sync` - Sync data between instances
- `POST /ECK/proxy` - Proxy requests to local instances

## Database

### PostgreSQL

Create database and user:

```sql
CREATE DATABASE eckwms_global;
CREATE USER eckwms_user WITH PASSWORD 'secure_password';
GRANT ALL PRIVILEGES ON DATABASE eckwms_global TO eckwms_user;
```

### Models

- **EckwmsInstance** - Registered eckWMS instances
- **RegisteredDevice** - Devices registered with instances
- **Scan** - QR code scan records

## Architecture

```
services/eckwms-global/
├── src/
│   ├── server.js           # Main entry point
│   ├── models/             # Sequelize models (LOCAL)
│   ├── utils/              # Utilities (LOCAL)
│   └── middleware/         # Express middleware
├── views/                  # EJS templates
├── config/                 # Configuration
├── package.json            # INDEPENDENT dependencies
└── .env.example            # Environment template
```

## Key Design Decisions

### 1. Independence
This microservice has **zero dependencies** on parent directory code:
- Models are localized in `src/models/`
- Utilities are localized in `src/utils/`
- Configuration is in `package.json` (not shared)
- Environment variables are service-specific

### 2. Database Options

**Option A: Dedicated Database** (Recommended)
- Each microservice has its own PostgreSQL database
- Eliminates coupling with main application
- Can scale independently
- Requires data sync via API for shared data

**Option B: Shared Database**
- Both services use same PostgreSQL database
- Simpler initial setup
- Risk of tight coupling
- Must manage schema changes carefully

**Option C: API Gateway**
- Microservice calls main app API instead of accessing database directly
- Ideal for strict isolation
- Adds network latency
- Cleaner separation of concerns

### 3. Encryption Key Management

The `ENC_KEY` must match the main app's key to decrypt QR codes. Options:

1. **Use main app's key**: Easy but couples encryption to main app
2. **Store in secrets manager**: Better security
3. **Use different keys**: If QR codes are created per-instance

## Testing

```bash
# Health check
curl http://localhost:8080/ECK/health

# API documentation
curl http://localhost:8080/

# Register instance (requires API key)
curl -X POST http://localhost:8080/ECK/api/internal/register-instance \
  -H "Content-Type: application/json" \
  -H "X-Internal-Api-Key: your_api_key" \
  -d '{"instanceId":"test-instance-1","localIps":["192.168.1.100"]}'
```

## Development

### Adding New Routes

1. Create route file in `src/routes/`
2. Import in `src/server.js`
3. Mount route with `app.use()`

Example:

```javascript
// src/routes/custom.js
const router = require('express').Router();
router.get('/status', (req, res) => res.json({ status: 'ok' }));
module.exports = router;

// src/server.js
const customRoutes = require('./routes/custom');
app.use('/custom', customRoutes);
```

### Adding New Models

1. Define model in `src/models/index.js`
2. Call `db.sequelize.sync()` to create tables
3. Use in endpoints

### Debugging

Enable detailed logging:

```bash
DB_LOGGING=true LOG_LEVEL=debug npm run dev
```

## Deployment

### PM2 (Recommended)

For production deployment, use PM2 as part of the main application stack.

See the root `ecosystem.config.js` and `PM2_SETUP.md` for detailed instructions.

```bash
# From root directory
pm2 start ecosystem.config.js --only eckwms-global

# Or just this microservice
pm2 start src/server.js --name "eckwms-global"
```

## Monitoring

### Health Check

```bash
curl http://localhost:8080/ECK/health
```

Response includes:
- Service status (healthy/degraded/unhealthy)
- Database connectivity
- Uptime
- Version

### Logs

```bash
# Local development
npm run dev

# PM2
pm2 logs eckwms-global
```

## Troubleshooting

### Port Already in Use

```bash
# Find and kill process on port 8080
lsof -i :8080
kill -9 <PID>
```

### Database Connection Failed

Check environment variables:
```bash
echo $PG_HOST $PG_DATABASE $PG_USERNAME
```

Ensure PostgreSQL is running and accessible.

### QR Code Decryption Fails

Verify `ENC_KEY` matches the encryption key used to create QR codes.

### Proxy Requests Fail

Ensure local instances are reachable at the URLs in connection candidates.

## Contributing

1. Follow existing code style
2. Add tests for new features
3. Update documentation
4. Test locally with `npm run dev` before submitting

## License

MIT - See LICENSE file

## Support

For issues and questions:
1. Check logs: `npm run dev`
2. Review `.env` configuration
3. Test endpoints manually with `curl`
4. Check database connectivity

---

**Status:** Production Ready ✓
**Last Updated:** 2025-12-03
