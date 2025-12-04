/**
 * PM2 Ecosystem Configuration
 *
 * Manages both pda.repair and eckwms-global services
 *
 * Usage:
 *   pm2 start ecosystem.config.js              # Start all services
 *   pm2 restart ecosystem.config.js            # Restart all services
 *   pm2 stop ecosystem.config.js               # Stop all services
 *   pm2 delete ecosystem.config.js             # Delete all services
 *   pm2 monit                                  # Monitor services
 *   pm2 logs                                   # View logs
 */

module.exports = {
  apps: [
    {
      // Main pda.repair application
      name: 'pda.repair',
      script: './app.js',
      cwd: '/var/www/pda.repair',
      instances: 1,
      exec_mode: 'fork',
      env: {
        PORT: 3000,
        NODE_ENV: 'production'
      },
      env_development: {
        NODE_ENV: 'development'
      },
      error_file: './logs/pda-repair-error.log',
      out_file: './logs/pda-repair-out.log',
      log_file: './logs/pda-repair-combined.log',
      time: true,
      watch: false,
      ignore_watch: ['node_modules', 'logs', '.git'],
      max_memory_restart: '500M',
      autorestart: true,
      max_restarts: 10,
      min_uptime: '10s'
    },

    {
      // eckWMS Global Server microservice
      name: 'eckwms-global',
      script: './src/server.js',
      cwd: '/var/www/pda.repair/services/eckwms-global',
      instances: 1,
      exec_mode: 'fork',
      env: {
        PORT: 8080,
        NODE_ENV: 'production',
        GLOBAL_SERVER_API_KEY: process.env.GLOBAL_SERVER_API_KEY || 'change_me_in_production',
        PG_HOST: process.env.PG_HOST || 'localhost',
        PG_PORT: process.env.PG_PORT || 5432,
        PG_DATABASE: process.env.PG_DATABASE || 'eckwms',
        PG_USERNAME: process.env.PG_USERNAME || 'wms_user',
        PG_PASSWORD: process.env.PG_PASSWORD || 'change_me',
        ENC_KEY: process.env.ENC_KEY || 'your_encryption_key',
        GLOBAL_SERVER_URL: process.env.GLOBAL_SERVER_URL || 'https://pda.repair',
        LOCAL_SERVER_PORT: 3000
      },
      env_development: {
        NODE_ENV: 'development',
        LOG_LEVEL: 'debug',
        DB_LOGGING: 'false'
      },
      error_file: '/var/www/pda.repair/logs/eckwms-global-error.log',
      out_file: '/var/www/pda.repair/logs/eckwms-global-out.log',
      log_file: '/var/www/pda.repair/logs/eckwms-global-combined.log',
      time: true,
      watch: false,
      ignore_watch: ['node_modules', 'logs', '.git'],
      max_memory_restart: '300M',
      autorestart: true,
      max_restarts: 10,
      min_uptime: '10s'
    }
  ],

  // Cluster configuration (optional)
  deploy: {
    production: {
      user: 'root',
      host: 'localhost',
      ref: 'origin/main',
      repo: 'git@github.com:your-org/pda-repair.git',
      path: '/var/www/pda.repair',
      'post-deploy': 'npm install && pm2 reload ecosystem.config.js --env production'
    }
  }
};
