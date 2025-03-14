// routes/auth.js
const express = require('express');
const router = express.Router();
const passport = require('passport');
const path = require('path');
const { generateTokens, refreshToken } = require('../middleware/auth');
const { UserAuth, RmaRequest } = require('../models/postgresql');
const { Sequelize } = require('sequelize');

// Serve HTML login page
router.get('/login', (req, res) => {
  res.sendFile(path.join(__dirname, '../html/login.html'));
});

// Serve HTML register page
router.get('/register', (req, res) => {
  res.sendFile(path.join(__dirname, '../html/register.html'));
});

// User registration
router.post('/register', async (req, res) => {
  try {
    const { username, email, password, name, company, phone } = req.body;
    
    // Check if user exists
    const existingUser = await UserAuth.findOne({ 
      where: { 
        [Sequelize.Op.or]: [
          { email },
          { username }
        ]
      }
    });
    
    if (existingUser) {
      return res.status(400).json({ error: 'User with this email or username already exists' });
    }
    
    // Create new user
    const newUser = await UserAuth.create({
      username,
      email,
      password, // Will be hashed by model hooks
      name,
      company,
      phone
    });
    
    // Generate tokens
    const tokens = generateTokens(newUser);
    
    res.status(201).json({ 
      tokens, 
      user: { 
        id: newUser.id, 
        username: newUser.username, 
        email: newUser.email,
        name: newUser.name
      } 
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Login
router.post('/login', (req, res, next) => {
  passport.authenticate('local', { session: false }, (err, user, info) => {
    if (err) {
      return next(err);
    }
    
    if (!user) {
      return res.status(401).json({ error: info.message || 'Invalid credentials' });
    }
    
    // Generate tokens
    const tokens = generateTokens(user);
    
    res.json({ 
      tokens, 
      user: { 
        id: user.id, 
        username: user.username, 
        email: user.email,
        name: user.name,
        role: user.role
      } 
    });
  })(req, res, next);
});

// Google OAuth login
router.get('/google', passport.authenticate('google', { 
  scope: ['profile', 'email'] 
}));

// Google OAuth callback
router.get('/google/callback', 
  passport.authenticate('google', { 
    failureRedirect: '/auth/login', 
    session: false 
  }),
  (req, res) => {
    // Generate tokens
    const tokens = generateTokens(req.user);
    
    // Create JWT token cookie (for web app)
    res.cookie('auth_token', tokens.accessToken, { 
      httpOnly: true,
      secure: process.env.NODE_ENV === 'production',
      maxAge: 3600000 // 1 hour
    });
    
    // Redirect to frontend with tokens
    res.redirect(`/auth-success?token=${tokens.accessToken}&refresh=${tokens.refreshToken}`);
  }
);

// Auth success page (to handle OAuth redirects)
router.get('/auth-success', (req, res) => {
  const token = req.query.token;
  const refresh = req.query.refresh;
  
  res.send(`
    <html>
      <head>
        <title>Authentication Successful</title>
        <script>
          // Store tokens in localStorage
          localStorage.setItem('auth_token', '` + token + `');
          localStorage.setItem('refresh_token', '` + refresh + `');
          
          // Redirect after 2 seconds
          setTimeout(() => {
            window.location.href = '/';
          }, 2000);
        </script>
        <style>
          body {
            font-family: Arial, sans-serif;
            text-align: center;
            padding-top: 50px;
          }
          .success-box {
            max-width: 500px;
            margin: 0 auto;
            padding: 30px;
            background-color: #f1f9f1;
            border-radius: 8px;
            box-shadow: 0 2px 10px rgba(0, 0, 0, 0.1);
          }
          h2 {
            color: #2c8a2c;
          }
        </style>
      </head>
      <body>
        <div class="success-box">
          <h2>Authentication Successful!</h2>
          <p>You've been successfully logged in.</p>
          <p>Redirecting to the homepage...</p>
        </div>
      </body>
    </html>
  `);
});

// Get current user info
router.get('/me', passport.authenticate('jwt', { session: false }), (req, res) => {
  res.json({
    id: req.user.id,
    username: req.user.username,
    email: req.user.email,
    name: req.user.name,
    role: req.user.role,
    company: req.user.company,
    phone: req.user.phone,
    street: req.user.street,
    houseNumber: req.user.houseNumber,
    postalCode: req.user.postalCode,
    city: req.user.city,
    country: req.user.country
  });
});

// Refresh token endpoint
router.post('/refresh-token', refreshToken);

// Get user's RMA requests
router.get('/rma-requests', passport.authenticate('jwt', { session: false }), async (req, res) => {
  try {
    const rmaRequests = await RmaRequest.findAll({
      where: { userId: req.user.id },
      order: [['createdAt', 'DESC']]
    });
    
    res.json(rmaRequests);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// User profile page
router.get('/profile', passport.authenticate('jwt', { session: false }), (req, res) => {
  res.send(`
    <html>
      <head>
        <title>User Profile - M3mobile</title>
        <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">
        <link rel="icon" type="image/png" sizes="32x32" href="/favicon-32x32.png">
        <link rel="icon" type="image/png" sizes="16x16" href="/favicon-16x16.png">
        <link rel="manifest" href="/site.webmanifest">
        <link rel="mask-icon" href="/safari-pinned-tab.svg" color="#5bbad5">
        
        <meta name="msapplication-TileColor" content="#da532c">
        <meta name="theme-color" content="#ffffff">
        <meta name="viewport" content="width=device-width, initial-scale=1.0, user-scalable=yes">
        <meta http-equiv="Content-Type" content="text/html; charset=utf-8" />
        
        <style>
          body {
            font-family: Arial, sans-serif;
            background: linear-gradient(#1e1e71ff 0px, #1e1e71ff 70px, #1e1e7100 300px, #8880),
              linear-gradient(-30deg, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881),
              linear-gradient(30deg, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881, #fff1, #8881);
            background-color: #b0b3c0;
            margin: 0;
            padding: 0;
          }
          
          .header-logo {
            padding: 10px;
            color: white;
            text-align: center;
            font-size: 24px;
            font-weight: bold;
          }
          
          .container {
            max-width: 800px;
            margin: 40px auto;
            padding: 20px;
            background-color: #fff;
            border-radius: 8px;
            box-shadow: 0 2px 10px rgba(0, 0, 0, 0.1);
          }
          
          .profile-section {
            margin-bottom: 30px;
          }
          
          .section-title {
            font-size: 20px;
            margin-bottom: 15px;
            color: #1e2071;
            border-bottom: 1px solid #eee;
            padding-bottom: 5px;
          }
          
          .profile-details {
            display: grid;
            grid-template-columns: 1fr 1fr;
            gap: 15px;
          }
          
          .detail-item {
            margin-bottom: 15px;
          }
          
          .detail-label {
            font-weight: bold;
            margin-bottom: 5px;
            color: #555;
          }
          
          .detail-value {
            padding: 8px;
            background-color: #f9f9f9;
            border-radius: 4px;
          }
          
          .rma-table {
            width: 100%;
            border-collapse: collapse;
            margin-top: 15px;
          }
          
          .rma-table th {
            background-color: #f1f1f1;
            text-align: left;
            padding: 10px;
          }
          
          .rma-table td {
            border-top: 1px solid #eee;
            padding: 10px;
          }
          
          .rma-table tr:hover {
            background-color: #f9f9f9;
          }
          
          .btn {
            background-color: #1e2071;
            color: white;
            border: none;
            padding: 10px 15px;
            font-size: 14px;
            border-radius: 4px;
            cursor: pointer;
            transition: background-color 0.3s;
            text-decoration: none;
            display: inline-block;
          }
          
          .btn:hover {
            background-color: #161a5e;
          }
          
          .btn-danger {
            background-color: #d9534f;
          }
          
          .btn-danger:hover {
            background-color: #c9302c;
          }
          
          .footer {
            text-align: center;
            margin-top: 30px;
          }
          
          .footer a {
            color: #1e2071;
            text-decoration: none;
            margin: 0 10px;
          }
        </style>
      </head>
      <body>
        <div class="header-logo">M3mobile</div>
        
        <div class="container">
          <h2 style="text-align: center; color: #1e2071;">User Profile</h2>
          
          <div class="profile-section">
            <h3 class="section-title">Personal Information</h3>
            <div class="profile-details">
              <div class="detail-item">
                <div class="detail-label">Username</div>
                <div class="detail-value" id="username">` + req.user.username + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">Name</div>
                <div class="detail-value" id="name">` + (req.user.name || '-') + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">Email</div>
                <div class="detail-value" id="email">` + req.user.email + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">Phone</div>
                <div class="detail-value" id="phone">` + (req.user.phone || '-') + `</div>
              </div>
            </div>
          </div>
          
          <div class="profile-section">
            <h3 class="section-title">Company Details</h3>
            <div class="profile-details">
              <div class="detail-item">
                <div class="detail-label">Company</div>
                <div class="detail-value" id="company">` + (req.user.company || '-') + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">Street</div>
                <div class="detail-value" id="street">` + (req.user.street || '-') + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">House Number</div>
                <div class="detail-value" id="houseNumber">` + (req.user.houseNumber || '-') + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">Postal Code</div>
                <div class="detail-value" id="postalCode">` + (req.user.postalCode || '-') + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">City</div>
                <div class="detail-value" id="city">` + (req.user.city || '-') + `</div>
              </div>
              <div class="detail-item">
                <div class="detail-label">Country</div>
                <div class="detail-value" id="country">` + (req.user.country || '-') + `</div>
              </div>
            </div>
          </div>
          
          <div class="profile-section">
            <h3 class="section-title">RMA Requests</h3>
            <div id="rma-list-container">
              <p>Loading your RMA requests...</p>
            </div>
          </div>
          
          <div class="footer">
            <a href="/" class="btn">Back to Home</a>
            <button id="logout-btn" class="btn btn-danger">Log Out</button>
          </div>
        </div>
        
        <script>
          // Fetch user's RMA requests
          async function fetchRMARequests() {
            try {
              const token = localStorage.getItem('auth_token');
              if (!token) {
                window.location.href = '/auth/login';
                return;
              }
              
              const response = await fetch('/auth/rma-requests', {
                headers: {
                  'Authorization': 'Bearer ' + token
                }
              });
              
              if (!response.ok) {
                throw new Error('Failed to fetch RMA requests');
              }
              
              const rmaRequests = await response.json();
              displayRMARequests(rmaRequests);
            } catch (err) {
              document.getElementById('rma-list-container').innerHTML = 
                '<p style="color: red;">Error loading RMA requests: ' + err.message + '</p>';
            }
          }
          
          // Display RMA requests in a table
          function displayRMARequests(requests) {
            const container = document.getElementById('rma-list-container');
            
            if (requests.length === 0) {
              container.innerHTML = '<p>You haven\\'t submitted any RMA requests yet.</p>';
              return;
            }
            
            let html = \`
              <table class="rma-table">
                <thead>
                  <tr>
                    <th>RMA Code</th>
                    <th>Date</th>
                    <th>Status</th>
                    <th>Actions</th>
                  </tr>
                </thead>
                <tbody>
            \`;
            
            requests.forEach(rma => {
              const date = new Date(rma.createdAt).toLocaleDateString();
              const status = getStatusLabel(rma);
              
              html += \`
                <tr>
                  <td>\${rma.rmaCode}</td>
                  <td>\${date}</td>
                  <td>\${status}</td>
                  <td>
                    <a href="/status/serial/\${rma.rmaCode}" class="btn" target="_blank">
                      View Details
                    </a>
                  </td>
                </tr>
              \`;
            });
            
            html += \`
                </tbody>
              </table>
            \`;
            
            container.innerHTML = html;
          }
          
          // Get human-readable status label
          function getStatusLabel(rma) {
            if (rma.shippedAt) return 'Shipped';
            if (rma.processedAt) return 'Processed';
            if (rma.receivedAt) return 'Received';
            return 'Created';
          }
          
          // Handle logout
          document.getElementById('logout-btn').addEventListener('click', async () => {
            try {
              const token = localStorage.getItem('auth_token');
              
              // Call logout endpoint
              await fetch('/auth/logout', {
                method: 'POST',
                headers: {
                  'Authorization': 'Bearer ' + token
                }
              });
              
              // Clear tokens
              localStorage.removeItem('auth_token');
              localStorage.removeItem('refresh_token');
              
              // Redirect to home
              window.location.href = '/';
            } catch (err) {
              console.error('Logout failed:', err);
              alert('Logout failed: ' + err.message);
            }
          });
          
          // Load RMA requests when page loads
          fetchRMARequests();
        </script>
      </body>
    </html>
  `);
});

// Logout
router.post('/logout', (req, res) => {
  // Clear cookies if any
  res.clearCookie('auth_token');
  res.json({ message: 'Logged out successfully' });
});

module.exports = router;