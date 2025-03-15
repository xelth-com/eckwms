// config/passport.js
const passport = require('passport');
const LocalStrategy = require('passport-local').Strategy;
const GoogleStrategy = require('passport-google-oauth20').Strategy;
const JwtStrategy = require('passport-jwt').Strategy;
const ExtractJwt = require('passport-jwt').ExtractJwt;
const { UserAuth } = require('../models/postgresql');
const bcrypt = require('bcrypt');

module.exports = function(secretJwt) {
  // Serialization (storing user in session)
  passport.serializeUser((user, done) => {
    done(null, user.id);
  });

  // Deserialization (retrieving user from session)
  passport.deserializeUser(async (id, done) => {
    try {
      const user = await UserAuth.findByPk(id);
      done(null, user);
    } catch (err) {
      done(err, null);
    }
  });

  // Local Strategy (email/password)
  passport.use(new LocalStrategy({ 
    usernameField: 'email',
    passwordField: 'password'
  }, async (email, password, done) => {
    try {
      const user = await UserAuth.findOne({ where: { email } });
      if (!user) {
        return done(null, false, { message: 'Invalid email or password' });
      }
      
      // Verify password
      const isMatch = await user.comparePassword(password);
      if (!isMatch) {
        return done(null, false, { message: 'Invalid email or password' });
      }
      
      // Update last login time
      user.lastLogin = new Date();
      await user.save();
      
      return done(null, user);
    } catch (err) {
      return done(err);
    }
  }));

  // Google OAuth Strategy
  if (process.env.GOOGLE_CLIENT_ID && process.env.GOOGLE_CLIENT_SECRET) {
    passport.use(new GoogleStrategy({
      clientID: process.env.GOOGLE_CLIENT_ID,
      clientSecret: process.env.GOOGLE_CLIENT_SECRET,
      callbackURL: '/auth/google/callback'
    }, async (accessToken, refreshToken, profile, done) => {
      try {
        // Check if user exists with Google ID
        let user = await UserAuth.findOne({ where: { googleId: profile.id } });
        
        if (user) {
          // Update last login time
          user.lastLogin = new Date();
          await user.save();
          return done(null, user);
        }
        
        // Check if user exists with email
        if (profile.emails && profile.emails.length > 0) {
          user = await UserAuth.findOne({ where: { email: profile.emails[0].value } });
          
          if (user) {
            // Link existing user to Google ID
            user.googleId = profile.id;
            user.lastLogin = new Date();
            await user.save();
            return done(null, user);
          }
        }
        
        // Create new user if not found
        const username = profile.displayName.replace(/\s/g, '').toLowerCase() + Math.floor(Math.random() * 1000);
        user = await UserAuth.create({
          googleId: profile.id,
          username: username,
          email: profile.emails ? profile.emails[0].value : `${username}@example.com`,
          name: profile.displayName,
          password: null // Password not needed for OAuth
        });
        
        return done(null, user);
      } catch (err) {
        return done(err);
      }
    }));
  }

  // JWT Strategy for API authentication
  passport.use(new JwtStrategy({
    jwtFromRequest: ExtractJwt.fromAuthHeaderAsBearerToken(),
    secretOrKey: global.secretJwt
  }, async (payload, done) => {
    try {
      // Find user by ID from payload
      const user = await UserAuth.findByPk(payload.userId);
      
      if (!user) {
        return done(null, false);
      }
      
      return done(null, user);
    } catch (err) {
      return done(err);
    }
  }));

  return passport;
};