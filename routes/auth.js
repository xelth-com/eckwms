// routes/auth.js
const express = require('express');
const router = express.Router();
const path = require('path');
const jwt = require('jsonwebtoken');
const passport = require('passport');
const { generateTokens, refreshToken, requireAdmin, optionalAuth, requireAuth } = require('../middleware/auth');
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

// routes/auth.js - Updated User registration route
router.post('/register', async (req, res) => {
  try {
    const { username, email, password, name, company, phone, street, city, postalCode, country, userType } = req.body;
    
    // Check if user exists with this email
    const existingEmail = await UserAuth.findOne({ 
      where: { email }
    });
    
    if (existingEmail) {
      return res.status(400).json({ error: 'Email already in use' });
    }
    
    // Check if username exists
    const existingUsername = await UserAuth.findOne({
      where: { username }
    });
    
    // If username exists, generate a unique username
    let finalUsername = username;
    if (existingUsername) {
      // Generate a unique username by adding a number
      const randomSuffix = Math.floor(Math.random() * 10000);
      finalUsername = `${username}${randomSuffix}`;
    }
    
    // Use the client-provided userType instead of deriving it
    const finalUserType = userType || 'individual';
    
    // Create new user
    const newUser = await UserAuth.create({
      username: finalUsername,
      email,
      password, // Will be hashed by model hooks
      name,
      company,
      phone,
      street,
      city,
      postalCode,
      country,
      userType: finalUserType, // Use client provided userType
      role: 'user'
    });
    
    // Generate tokens
    const tokens = generateTokens(newUser);
    
    res.status(201).json({ 
      tokens, 
      user: { 
        id: newUser.id, 
        username: newUser.username, 
        email: newUser.email,
        name: newUser.name,
        company: newUser.company,
        userType: newUser.userType,
        role: newUser.role
      } 
    });
  } catch (error) {
    console.error('Registration error:', error);
    res.status(500).json({ error: error.message });
  }
});

// Login - supports both email and username login
router.post('/login', async (req, res, next) => {
  try {
    const { email, password } = req.body;
    
    // Determine if input is email or username
    const isEmail = email && email.includes('@');
    const whereCondition = isEmail ? { email } : { username: email };
    
    // Find users matching the login
    const users = await UserAuth.findAll({ where: whereCondition });
    
    if (users.length === 0) {
      return res.status(401).json({ error: 'Invalid credentials' });
    }
    
    // If only one user found, simple case
    if (users.length === 1) {
      const user = users[0];
      const isMatch = await user.comparePassword(password);
      
      if (!isMatch) {
        return res.status(401).json({ error: 'Invalid credentials' });
      }
      
      // Update last login
      user.lastLogin = new Date();
      await user.save();
      
      // Generate tokens
      const tokens = generateTokens(user);
      
      return res.json({
        tokens,
        user: {
          id: user.id,
          username: user.username,
          email: user.email,
          name: user.name,
          role: user.role,
          userType: user.userType,
          company: user.company
        }
      });
    }
    
    // Multiple users with same username (but different emails)
    // Try to find one that matches the password
    for (const user of users) {
      const isMatch = await user.comparePassword(password);
      
      if (isMatch) {
        // Update last login
        user.lastLogin = new Date();
        await user.save();
    
    // Generate tokens
    const tokens = generateTokens(user);
    
        return res.json({
      tokens, 
      user: { 
        id: user.id, 
        username: user.username, 
        email: user.email,
        name: user.name,
            role: user.role,
            userType: user.userType,
            company: user.company
          }
        });
      }
    }
    
    // No matching password found
    return res.status(401).json({ error: 'Invalid credentials' });
    
  } catch (error) {
    return res.status(500).json({ error: error.message });
  }
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

// Get current user info
router.get('/me', requireAuth, (req, res) => {
  res.json({
    id: req.user.id,
    username: req.user.username,
    email: req.user.email,
    name: req.user.name,
    role: req.user.role,
    userType: req.user.userType,
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
router.get('/rma-requests', requireAuth, async (req, res) => {
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

// Create admin user (admin only)
router.post('/create-admin', requireAdmin, async (req, res) => {
  try {
    const { username, email, password, name } = req.body;
    
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
    
    // Create new admin user
    const newAdmin = await UserAuth.create({
      username,
      email,
      password, // Will be hashed by model hooks
      name,
      role: 'admin',
      userType: 'individual'
    });
    
    res.status(201).json({
      message: 'Admin user created successfully',
      user: {
        id: newAdmin.id,
        username: newAdmin.username,
        email: newAdmin.email,
        name: newAdmin.name,
        role: newAdmin.role
      }
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Upgrade RMA user to regular user
router.post('/upgrade-rma-user', async (req, res) => {
  try {
    const { email, password, username } = req.body;
    
    // Find the RMA user
    const rmaUser = await UserAuth.findOne({
      where: {
        email,
        role: 'rma'
      }
    });
    
    if (!rmaUser) {
      return res.status(404).json({ error: 'RMA user not found' });
    }
    
    // Check if the username already exists
    let finalUsername = username;
    const existingUsername = await UserAuth.findOne({
      where: { username }
    });
    
    if (existingUsername) {
      const randomSuffix = Math.floor(Math.random() * 10000);
      finalUsername = `${username}${randomSuffix}`;
    }
    
    // Update the RMA user to a regular user
    rmaUser.username = finalUsername;
    rmaUser.password = password; // Will be hashed by model hooks
    rmaUser.role = 'user';
    
    await rmaUser.save();
    
    // Generate tokens
    const tokens = generateTokens(rmaUser);
    
    res.json({
      tokens,
      user: {
        id: rmaUser.id,
        username: rmaUser.username,
        email: rmaUser.email,
        name: rmaUser.name,
        role: rmaUser.role,
        userType: rmaUser.userType,
        company: rmaUser.company
      }
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Logout
router.post('/logout', (req, res) => {
  // Clear cookies if any
  res.clearCookie('auth_token');
  res.json({ message: 'Logged out successfully' });
});

module.exports = router;