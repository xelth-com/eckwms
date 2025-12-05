/**
 * PM2 Ecosystem Configuration for eckWMS Global Server
 *
 * Usage:
 *   pm2 start ecosystem.config.js
 *   pm2 restart eckwms-global
 *   pm2 logs eckwms-global
 *   pm2 stop eckwms-global
 */

module.exports = {
  apps: [
    {
      name: 'eckwms-global',
      script: './src/server.js',
      cwd: '/var/www/eckwms/services/eckwms-global',
      instances: 1,
      exec_mode: 'fork',
      env: {
        PORT: 8080,
        NODE_ENV: 'production'
      },
      env_development: {
        NODE_ENV: 'development',
        LOG_LEVEL: 'debug'
      },
      error_file: './logs/eckwms-global-error.log',
      out_file: './logs/eckwms-global-out.log',
      log_file: './logs/eckwms-global-combined.log',
      time: true,
      watch: false,
      ignore_watch: ['node_modules', 'logs', '.git'],
      max_memory_restart: '300M',
      autorestart: true,
      max_restarts: 10,
      min_uptime: '10s'
    }
  ]
};
