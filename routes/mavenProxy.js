// routes/mavenProxy.js
const express = require('express');
const router = express.Router();
const http = require('http');
const { Buffer } = require('node:buffer');

/**
 * Maven proxy route for securely accessing an insecure Maven repository
 * Handles proxying from nginx to the Maven repository
 */

// Original Maven repository host 
const MAVEN_TARGET_HOST = '47.108.228.164';
const MAVEN_TARGET_PORT = 8081;
const MAVEN_TARGET_URL = `http://${MAVEN_TARGET_HOST}:${MAVEN_TARGET_PORT}`;

// Proxy host (our secure host)
const PROXY_HOST = 'pda.repair';
const PROXY_URL = `https://${PROXY_HOST}:8081`;

/**
 * Proxy middleware to forward Maven repository requests
 * and replace URLs in responses
 */
router.use('*', (req, res) => {
  // Get the original path
  const originalPath = req.originalUrl;
  
  // Log the incoming request
  console.log(`[Maven Proxy] Incoming request: ${originalPath}`);
  
  // Options for the outgoing request
  const options = {
    hostname: MAVEN_TARGET_HOST,
    port: MAVEN_TARGET_PORT,
    path: originalPath,
    method: req.method,
    headers: {
      ...req.headers,
      host: `${MAVEN_TARGET_HOST}:${MAVEN_TARGET_PORT}` // Set the correct host header
    }
  };
  
  console.log(`[Maven Proxy] Forwarding to: ${MAVEN_TARGET_URL}${originalPath}`);
  
  // Send request to the target Maven repository
  const proxyReq = http.request(options, (proxyRes) => {
    // Handle cookies and preserve headers
    Object.keys(proxyRes.headers).forEach(key => {
      let value = proxyRes.headers[key];
      if (key === 'location' || key === 'Location') {
        // Rewrite location headers if needed
        value = value.replace(new RegExp(MAVEN_TARGET_URL, 'g'), PROXY_URL);
      }
      res.setHeader(key, value);
    });
    
    res.statusCode = proxyRes.statusCode;
    
    // For binary files, stream directly without modification
    const contentType = proxyRes.headers['content-type'] || '';
    if (contentType.includes('application/java-archive') || 
        contentType.includes('application/octet-stream') || 
        contentType.includes('application/zip') ||
        originalPath.endsWith('.jar') || originalPath.endsWith('.pom') || 
        originalPath.endsWith('.md5') || originalPath.endsWith('.sha1')) {
      
      proxyRes.pipe(res);
      return;
    }
    
    // For text responses, collect and modify content
    let responseBody = Buffer.from('');
    proxyRes.on('data', (chunk) => {
      responseBody = Buffer.concat([responseBody, chunk]);
    });
    
    proxyRes.on('end', () => {
      try {
        // Convert to string for text-based content
        const bodyStr = responseBody.toString('utf8');
        
        let modifiedBody = bodyStr;
        
        // Replace all variations of the URLs in resourceURI tags
        // This is the critical fix - the original host could be returned in multiple forms
        
        // 1. HTTP URLs
        modifiedBody = modifiedBody.replace(
          new RegExp(`http://${MAVEN_TARGET_HOST}:${MAVEN_TARGET_PORT}`, 'g'), 
          `https://${PROXY_HOST}:8081`
        );
        
        // 2. HTTPS URLs (already showing up in the current output)
        modifiedBody = modifiedBody.replace(
          new RegExp(`https://${MAVEN_TARGET_HOST}:${MAVEN_TARGET_PORT}`, 'g'), 
          `https://${PROXY_HOST}:8081`
        );
        
        // 3. Just to be safe, also replace the IP without protocol
        modifiedBody = modifiedBody.replace(
          new RegExp(`${MAVEN_TARGET_HOST}:${MAVEN_TARGET_PORT}`, 'g'), 
          `${PROXY_HOST}:8081`
        );
        
        // 4. Handle resourceURI specific tags (current issue)
        modifiedBody = modifiedBody.replace(
          /<resourceURI>https?:\/\/47\.108\.228\.164:8081/g,
          `<resourceURI>https://${PROXY_HOST}:8081`
        );
        
        console.log('[Maven Proxy] URL replacements complete');
        
        // Set correct content length and send response
        res.setHeader('content-length', Buffer.byteLength(modifiedBody));
        res.end(modifiedBody);
      } catch (error) {
        console.error('[Maven Proxy] Error modifying response:', error);
        res.end(responseBody);
      }
    });
  });
  
  // Handle request errors
  proxyReq.on('error', (error) => {
    console.error('[Maven Proxy] Proxy request error:', error);
    res.statusCode = 502;
    res.end(`Proxy Error: ${error.message}`);
  });
  
  // Forward the request body if present
  if (['POST', 'PUT', 'PATCH'].includes(req.method)) {
    req.pipe(proxyReq);
  } else {
    proxyReq.end();
  }
});

module.exports = router;