# Development Journal

## Recent Changes
Track significant changes, decisions, and progress here.

---

### 2025-12-26 - Automatic JWT Token Refresh

**Type**: Feature | **Scope**: Frontend Auth | **By**: Claude Code

**What**: Implemented automatic token refreshing to prevent session timeouts

**Changes**:
- Created `public/js/auth-client.js` - fetch wrapper with 401 interceptor
- Fixed token naming bugs (refreshToken vs refresh_token)
- Integrated into `/admin/pairing` and `/admin/printing` pages
- Created `.eck/AUTH_INTEGRATION_GUIDE.md` - developer documentation

**Impact**: Users no longer get logged out when access token expires (1h)

**Testing**: ✅ Verified on Device Pairing page - works perfectly

**Documentation**: See `.eck/AUTH_INTEGRATION_GUIDE.md` for integration guide

---

### YYYY-MM-DD - Project Started
- Initial project setup
- Added basic structure
