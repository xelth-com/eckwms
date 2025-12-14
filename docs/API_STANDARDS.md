# API Standards & QR Code Optimization

## Uppercase URL Convention

To maximize data density in QR codes (utilizing **Alphanumeric mode** instead of Byte mode), all URLs and API paths MUST be uppercase.

### QR Code Encoding Modes

QR codes support different encoding modes with varying efficiency:

- **Numeric mode**: 10 bits per 3 digits (0-9 only)
- **Alphanumeric mode**: 11 bits per 2 characters (0-9, A-Z, space, $%*+-./:)
- **Byte mode**: 8 bits per character (any character)

By using uppercase letters and avoiding lowercase, we enable **Alphanumeric mode**, which is approximately **45% more efficient** than Byte mode for text data.

### Base URL

```
HTTPS://PDA.REPAIR/ECK
```

### API Endpoints

All endpoints follow the uppercase convention:

#### Device Management
- `POST /ECK/API/DEVICE/REGISTER` - Register a new device with Ed25519 signature

#### Scan Operations
- `GET /ECK/API/SCANS` - Retrieve recent scans (public demo)
- `POST /ECK/API/SCAN` - Submit a new scan (requires X-API-Key)
- `GET /ECK/API/PULL` - Pull buffered scans (requires X-API-Key)
- `POST /ECK/API/CONFIRM` - Confirm scan receipt (requires X-API-Key)

#### File Upload
- `POST /ECK/API/UPLOAD/IMAGE` - Upload image file

### Implementation Notes

**Important:** While HTTP schemes (`HTTPS://`) and domain names (`PDA.REPAIR`) are case-insensitive by RFC standards, the path component (`/ECK/API/...`) is case-sensitive on most web servers, including our Express.js setup.

We enforce uppercase everywhere for:
1. **Consistency** - Uniform convention across all components
2. **QR Optimization** - Enable Alphanumeric encoding mode
3. **Readability** - Clear visual distinction in logs and debugging

### Example QR Code Payloads

#### Device Pairing
```json
{
  "type": "eckwms-pairing",
  "version": "1.0",
  "serverUrl": "HTTPS://PDA.REPAIR/ECK",
  "serverPublicKey": "INC5MLXBWBD8P0/TH5Z20/WPPG8IDXMGYDOY1AXNIWI="
}
```

#### Item Tracking
```
HTTPS://PDA.REPAIR/ECK/ABC123XYZ
```

### Character Set Compatibility

The Alphanumeric mode supports these characters:
```
0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:
```

**Avoid in QR payloads:**
- Lowercase letters (a-z)
- Special characters not in the alphanumeric set
- Unicode/emoji characters

### Migration Checklist

- [x] Update all route definitions to uppercase
- [x] Update serverUrl in QR code generation
- [x] Update API documentation
- [ ] Update client applications to use uppercase endpoints
- [ ] Update test suites
- [ ] Update monitoring/logging filters

### References

- [QR Code Encoding Modes - Wikipedia](https://en.wikipedia.org/wiki/QR_code#Encoding)
- [ISO/IEC 18004:2015 - QR Code Standard](https://www.iso.org/standard/62021.html)
