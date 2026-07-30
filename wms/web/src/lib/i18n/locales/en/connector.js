// MCP Connector admin page: download a pre-configured stdio shim bundle, or
// wire a remote-MCP-capable client straight at this server. English source strings.
export default {
  title: 'MCP Connector',
  description: 'Connect an AI client (Claude Code, Antigravity, …) to this WMS. Download a pre-configured connector bundle, or point a client that supports remote MCP directly at this server.',

  f_platform: 'Platform',
  platform_windows: 'Windows',
  platform_linux: 'Linux',

  f_tier: 'Access tier',
  tier_agent: 'Agent — PII stays pseudonymized',
  tier_master: 'Master — can restore PII into files',

  f_config_only: 'Config only (no binary)',

  btn_download: 'Download bundle',
  downloading: 'Downloading…',
  toast_download_success: 'Bundle downloaded',
  toast_download_failed: 'Download failed: {error}',

  direct_title: 'Direct connection (no shim)',
  direct_desc: 'If your MCP client supports remote HTTP transport, skip the download and connect straight to this server:',
  direct_token_note: 'the token is inside the downloaded bundle\'s shim.env',
  copy: 'Copy',
  copied: 'Copied!',
};
