// routes/dashboard.js
const express = require('express');
const router = express.Router();
const passport = require('passport');
const { UserAuth, RmaRequest } = require('../../../shared/models/postgresql');
const { requireAdmin, requireAuth } = require('../middleware/auth');
const { Sequelize } = require('sequelize');

// User dashboard - based on user type
router.get('/', requireAuth, async (req, res) => {
  try {
    let dashboardHtml = '';
    
    // Base dashboard HTML with user info
    const baseHtml = `
      <div class="dashboard-header">
        <h2>Welcome, ${req.user.name || req.user.username}</h2>
        <p>User Type: ${req.user.userType === 'company' ? 'Company' : 'Individual'}</p>
        <p>Role: ${req.user.role}</p>
      </div>
    `;
    
    // Add specific content based on user role
    if (req.user.role === 'admin') {
      // Admin dashboard
      dashboardHtml = `
        ${baseHtml}
        <div class="dashboard-section">
          <h3>Admin Panel</h3>
          <div class="admin-links">
            <a href="/admin/users" class="admin-link">Manage Users</a>
            <a href="/admin/rma-requests" class="admin-link">All RMA Requests</a>
            <a href="/admin/generate-codes" class="admin-link">Generate Codes</a>
            <a href="/admin/stats" class="admin-link">System Stats</a>
          </div>
        </div>
      `;
    } else if (req.user.role === 'rma') {
      // RMA user dashboard - prompt to upgrade
      dashboardHtml = `
        ${baseHtml}
        <div class="dashboard-section upgrade-account">
          <h3>Complete Your Account</h3>
          <p>Your account was created from an RMA submission. Complete your registration to access more features.</p>
          <a href="/auth/upgrade-rma-account" class="btn">Complete Registration</a>
        </div>
        
        <div class="dashboard-section">
          <h3>Your RMA Requests</h3>
          ${await generateRmaRequestsHtml(req.user.id)}
        </div>
      `;
    } else {
      // Regular user dashboard
      dashboardHtml = `
        ${baseHtml}
        <div class="dashboard-section">
          <h3>Your RMA Requests</h3>
          ${await generateRmaRequestsHtml(req.user.id)}
        </div>
        
        <div class="dashboard-section">
          <h3>Quick Actions</h3>
          <div class="quick-actions">
            <a href="/auth/link-rma" class="quick-action">Link Existing RMA</a>
            <a href="/auth/profile" class="quick-action">Edit Profile</a>
          </div>
        </div>
      `;
    }
    
    // Return the complete HTML with styling
    res.send(`
      <!DOCTYPE html>
      <html>
      <head>
        <title>Dashboard - M3mobile</title>
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
            max-width: 1000px;
            margin: 40px auto;
            padding: 20px;
            background-color: #fff;
            border-radius: 8px;
            box-shadow: 0 2px 10px rgba(0, 0, 0, 0.1);
          }
          
          .dashboard-header {
            margin-bottom: 30px;
            padding-bottom: 15px;
            border-bottom: 1px solid #eee;
          }
          
          .dashboard-header h2 {
            color: #1e2071;
            margin-bottom: 10px;
          }
          
          .dashboard-section {
            margin-bottom: 30px;
            padding: 20px;
            background-color: #f9f9f9;
            border-radius: 6px;
          }
          
          .dashboard-section h3 {
            color: #333;
            margin-top: 0;
            margin-bottom: 15px;
          }
          
          .admin-links,
          .quick-actions {
            display: grid;
            grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
            gap: 15px;
            margin-top: 15px;
          }
          
          .admin-link,
          .quick-action {
            display: block;
            padding: 15px;
            background-color: #e6f7ff;
            color: #1e2071;
            text-decoration: none;
            text-align: center;
            border-radius: 4px;
            transition: background-color 0.3s;
          }
          
          .admin-link:hover,
          .quick-action:hover {
            background-color: #cce5ff;
          }
          
          .rma-table {
            width: 100%;
            border-collapse: collapse;
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
            background-color: #f5f5f5;
          }
          
          .btn {
            display: inline-block;
            background-color: #1e2071;
            color: white;
            padding: 10px 15px;
            text-decoration: none;
            border-radius: 4px;
            border: none;
            cursor: pointer;
          }
          
          .btn:hover {
            background-color: #161a5e;
          }
          
          .upgrade-account {
            background-color: #fff7e6;
            border: 1px solid #ffd591;
          }
          
          .status-badge {
            display: inline-block;
            padding: 4px 8px;
            border-radius: 4px;
            font-size: 12px;
            text-transform: uppercase;
          }
          
          .status-created {
            background-color: #e6f7ff;
            color: #1890ff;
          }
          
          .status-received {
            background-color: #fff7e6;
            color: #fa8c16;
          }
          
          .status-processed {
            background-color: #f6ffed;
            color: #52c41a;
          }
          
          .status-shipped {
            background-color: #f9f0ff;
            color: #722ed1;
          }
          
          .footer {
            text-align: center;
            margin-top: 30px;
            padding-top: 15px;
            border-top: 1px solid #eee;
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
          ${dashboardHtml}
          
          <div class="footer">
            <a href="/">Back to Home</a>
            <a href="#" id="logout-btn">Log Out</a>
          </div>
        </div>
        
        <script>
          document.getElementById('logout-btn').addEventListener('click', async (e) => {
            e.preventDefault();
            
            try {
              const token = localStorage.getItem('auth_token');
              
              await fetch('/auth/logout', {
                method: 'POST',
                headers: {
                  'Authorization': 'Bearer ' + token
                }
              });
              
              localStorage.removeItem('auth_token');
              localStorage.removeItem('refresh_token');
              
              window.location.href = '/';
            } catch (err) {
              console.error('Logout failed:', err);
            }
          });
        </script>
      </body>
      </html>
    `);
  } catch (error) {
    res.status(500).send(`Error loading dashboard: ${error.message}`);
  }
});

// Admin user management
router.get('/admin/users', requireAdmin, async (req, res) => {
  try {
    const users = await UserAuth.findAll({
      attributes: ['id', 'username', 'email', 'name', 'company', 'role', 'userType', 'createdAt', 'lastLogin', 'isActive'],
      order: [['createdAt', 'DESC']]
    });
    
    let userRows = '';
    
    users.forEach(user => {
      const lastLogin = user.lastLogin ? new Date(user.lastLogin).toLocaleString() : 'Never';
      const createdAt = new Date(user.createdAt).toLocaleString();
      
      userRows += `
        <tr>
          <td>${user.username}</td>
          <td>${user.email}</td>
          <td>${user.name || '-'}</td>
          <td>${user.company || '-'}</td>
          <td>
            <span class="role-badge role-${user.role}">${user.role}</span>
          </td>
          <td>${user.userType}</td>
          <td>${createdAt}</td>
          <td>${lastLogin}</td>
          <td>
            <span class="status-badge ${user.isActive ? 'status-active' : 'status-inactive'}">
              ${user.isActive ? 'Active' : 'Inactive'}
            </span>
          </td>
          <td>
            <a href="/dashboard/admin/users/${user.id}" class="btn btn-sm">Edit</a>
          </td>
        </tr>
      `;
    });
    
    res.send(`
      <!DOCTYPE html>
      <html>
      <head>
        <title>User Management - M3mobile</title>
        <!-- Head content -->
        <style>
          /* CSS styles */
          .role-badge {
            display: inline-block;
            padding: 4px 8px;
            border-radius: 4px;
            font-size: 12px;
            text-transform: uppercase;
          }
          
          .role-admin {
            background-color: #f6ffed;
            color: #52c41a;
          }
          
          .role-user {
            background-color: #e6f7ff;
            color: #1890ff;
          }
          
          .role-rma {
            background-color: #fff7e6;
            color: #fa8c16;
          }
          
          .status-active {
            background-color: #f6ffed;
            color: #52c41a;
          }
          
          .status-inactive {
            background-color: #fff1f0;
            color: #f5222d;
          }
          
          .btn-sm {
            padding: 4px 8px;
            font-size: 12px;
          }
          
          .user-table {
            width: 100%;
            border-collapse: collapse;
          }
          
          .user-table th {
            background-color: #f1f1f1;
            text-align: left;
            padding: 10px;
          }
          
          .user-table td {
            border-top: 1px solid #eee;
            padding: 10px;
          }
          
          .create-user-btn {
            margin-bottom: 20px;
          }
        </style>
      </head>
      <body>
        <div class="header-logo">M3mobile</div>
        
        <div class="container">
          <div class="dashboard-header">
            <h2>User Management</h2>
            <a href="/dashboard" class="btn">Back to Dashboard</a>
          </div>
          
          <div class="create-user-btn">
            <a href="/dashboard/admin/users/create" class="btn">Create New User</a>
          </div>
          
          <table class="user-table">
            <thead>
              <tr>
                <th>Username</th>
                <th>Email</th>
                <th>Name</th>
                <th>Company</th>
                <th>Role</th>
                <th>User Type</th>
                <th>Created</th>
                <th>Last Login</th>
                <th>Status</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              ${userRows}
            </tbody>
          </table>
          
          <div class="footer">
            <a href="/dashboard">Back to Dashboard</a>
          </div>
        </div>
      </body>
      </html>
    `);
  } catch (error) {
    res.status(500).send(`Error loading users: ${error.message}`);
  }
});

// Helper function to generate RMA requests HTML
async function generateRmaRequestsHtml(userId) {
  try {
    const rmaRequests = await RmaRequest.findAll({
      where: { userId },
      order: [['createdAt', 'DESC']]
    });
    
    if (rmaRequests.length === 0) {
      return '<p>You have no RMA requests yet.</p>';
    }
    
    let rmaRows = '';
    
    rmaRequests.forEach(rma => {
      const createdAt = new Date(rma.createdAt).toLocaleDateString();
      
      // Determine status
      let status = 'Created';
      let statusClass = 'status-created';
      
      if (rma.shippedAt) {
        status = 'Shipped';
        statusClass = 'status-shipped';
      } else if (rma.processedAt) {
        status = 'Processed';
        statusClass = 'status-processed';
      } else if (rma.receivedAt) {
        status = 'Received';
        statusClass = 'status-received';
      }
      
      rmaRows += `
        <tr>
          <td>${rma.rmaCode}</td>
          <td>${createdAt}</td>
          <td>
            <span class="status-badge ${statusClass}">
              ${status}
            </span>
          </td>
          <td>
            <a href="/status/serial/${rma.rmaCode}" class="btn" target="_blank">
              View Details
            </a>
          </td>
        </tr>
      `;
    });
    
    return `
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
          ${rmaRows}
        </tbody>
      </table>
    `;
  } catch (error) {
    console.error('Error generating RMA HTML:', error);
    return `<p>Error loading RMA requests: ${error.message}</p>`;
  }
}

// Link existing RMA to user account
router.post('/link-rma', requireAuth, async (req, res) => {
  try {
    const { rmaCode, email } = req.body;
    
    if (!rmaCode || !email) {
      return res.status(400).json({ error: 'RMA code and email are required' });
    }
    
    // Find RMA by code
    const rmaRequest = await RmaRequest.findOne({
      where: { 
        rmaCode: rmaCode,
        email: email
      }
    });
    
    if (!rmaRequest) {
      return res.status(404).json({ error: 'RMA not found or email does not match' });
    }
    
    // Update RMA to link it to the current user
    rmaRequest.userId = req.user.id;
    await rmaRequest.save();
    
    // If there's an RMA user with this email, merge data
    const rmaUser = await UserAuth.findOne({
      where: {
        email: email,
        role: 'rma'
      }
    });
    
    if (rmaUser && rmaUser.id !== req.user.id) {
      // Link all RMAs from this RMA user to the current user
      await RmaRequest.update(
        { userId: req.user.id },
        { where: { userId: rmaUser.id } }
      );
      
      // Deactivate the RMA user
      rmaUser.isActive = false;
      await rmaUser.save();
    }
    
    res.json({ success: true, message: 'RMA linked to your account successfully' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

module.exports = router;