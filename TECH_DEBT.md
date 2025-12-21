# Technical Debt & Limitations

## SDUI Engine (Android)
- **Components**: Currently supports only `Text`, `Button`, `Card`, `Spacing`. Needs `TextInput`, `Image`, `Dropdown`.
- **State Management**: Dynamic UI is stateless. Form inputs are not yet sent back to the server automatically.
- **Navigation**: Simple back stack. No support for complex navigation graphs within JSON.

## RBAC
- **Permission Granularity**: Permissions are checked only at UI entry points. Need deeper integration into API endpoints.
