// middleware/contentTypeDebug.js
/**
 * Middleware to ensure proper Content-Type for HTML responses
 */
module.exports = function contentTypeDebugMiddleware() {
  return (req, res, next) => {
    // Store the original send method
    const originalSend = res.send;
    
    // Override the send method
    res.send = function(body) {
      // Check if content is likely HTML but has no Content-Type set
      if (typeof body === 'string' && 
         (body.includes('<!DOCTYPE html>') || 
          body.includes('<html>') || 
          body.includes('<head>') || 
          body.includes('<body>')) && 
         !res.get('Content-Type')) {
        
        // Set Content-Type to text/html if not already set
        console.log('[debug] Setting missing Content-Type: text/html');
        res.type('html');
      }
      
      // Call the original send method
      return originalSend.call(this, body);
    };
    
    next();
  };
};