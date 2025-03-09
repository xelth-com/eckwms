// server.js
require('dotenv').config();
const http = require('http');
const app = require('./app');
const logger = require('./utils/logging');

// Get port from environment and store in Express
const port = normalizePort(process.env.PORT || '3000');
app.set('port', port);

// Create HTTP server
const server = http.createServer(app);

// Listen on provided port, on all network interfaces.
server.listen(port);
server.on('error', onError);
server.on('listening', onListening);

// Normalize a port into a number, string, or false.
function normalizePort(val) {
  const port = parseInt(val, 10);

  if (isNaN(port)) {
    // named pipe
    return val;
  }

  if (port >= 0) {
    // port number
    return port;
  }

  return false;
}

// Event listener for HTTP server "error" event.
function onError(error) {
  if (error.syscall !== 'listen') {
    throw error;
  }

  const bind = typeof port === 'string'
    ? 'Pipe ' + port
    : 'Port ' + port;

  // handle specific listen errors with friendly messages
  switch (error.code) {
    case 'EACCES':
      logger.error(bind + ' requires elevated privileges');
      process.exit(1);
      break;
    case 'EADDRINUSE':
      logger.error(bind + ' is already in use');
      process.exit(1);
      break;
    default:
      throw error;
  }
}

// Event listener for HTTP server "listening" event.
function onListening() {
  const addr = server.address();
  const bind = typeof addr === 'string'
    ? 'pipe ' + addr
    : 'port ' + addr.port;
  logger.info('Server listening on ' + bind);
  
  // Log server info
  logger.info(`Environment: ${process.env.NODE_ENV || 'development'}`);
  logger.info(`Application version: ${process.env.npm_package_version || '1.0.0'}`);
}

// Graceful shutdown
process.on('SIGTERM', gracefulShutdown);
process.on('SIGINT', gracefulShutdown);

async function gracefulShutdown() {
  logger.info('SIGTERM/SIGINT received. Shutting down gracefully...');
  
  // Close the server to stop accepting new connections
  server.close(() => {
    logger.info('HTTP server closed');
    
    // Additional cleanup tasks can go here:
    // - Close database connections
    // - Release other resources
    
    process.exit(0);
  });
  
  // Force close if graceful shutdown takes too long
  setTimeout(() => {
    logger.error('Forcing shutdown after timeout');
    process.exit(1);
  }, 10000); // 10 seconds
}