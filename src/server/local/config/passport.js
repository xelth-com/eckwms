// config/passport.js
const passport = require('passport');
const LocalStrategy = require('passport-local').Strategy;
const GoogleStrategy = require('passport-google-oauth20').Strategy;
const JwtStrategy = require('passport-jwt').Strategy;
const ExtractJwt = require('passport-jwt').ExtractJwt;
const { UserAuth } = require('../../shared/models/postgresql');
const bcrypt = require('bcrypt'); // Если не используется здесь, можно удалить
const { Buffer } = require('node:buffer'); // Импортируем Buffer

// --- НАЧАЛО: Инициализация ключа для Passport ---

// Читаем секрет из переменной окружения ПРЯМО ЗДЕСЬ
const jwtSecretHexForPassport = process.env.JWT_SECRET;

// Проверяем наличие секрета
if (!jwtSecretHexForPassport) {
    // Эта ошибка остановит запуск приложения, если ключ не найден
    throw new Error('КРИТИЧЕСКАЯ ОШИБКА: Переменная окружения JWT_SECRET не установлена! Невозможно настроить JWT стратегию Passport.');
}

// Создаем буфер из секрета для использования в JwtStrategy
// (Используем Buffer, как наиболее вероятный совместимый тип для passport-jwt)
const jwtSecretBufferForPassport = Buffer.from(jwtSecretHexForPassport, 'hex');
console.log('JWT Secret Buffer для Passport инициализирован в config/passport.js'); // Лог для подтверждения

// --- КОНЕЦ: Инициализация ключа для Passport ---


// Функция больше НЕ ПРИНИМАЕТ secretJwt как аргумент
module.exports = function() {
  // Serialization (storing user in session)
  passport.serializeUser((user, done) => {
    done(null, user.id);
  });

  // Deserialization (retrieving user from session)
  passport.deserializeUser(async (id, done) => {
    try {
      const user = await UserAuth.findByPk(id);
      done(null, user); // Передаем null в ошибку, user в результат
    } catch (err) {
      done(err, null); // Передаем ошибку, null в результат
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
      const isMatch = await user.comparePassword(password);
      if (!isMatch) {
        return done(null, false, { message: 'Invalid email or password' });
      }
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
        let user = await UserAuth.findOne({ where: { googleId: profile.id } });
        if (user) {
          user.lastLogin = new Date();
          await user.save();
          return done(null, user);
        }
        if (profile.emails && profile.emails.length > 0) {
          user = await UserAuth.findOne({ where: { email: profile.emails[0].value } });
          if (user) {
            user.googleId = profile.id;
            user.lastLogin = new Date();
            await user.save();
            return done(null, user);
          }
        }
        const username = profile.displayName.replace(/\s/g, '').toLowerCase() + Math.floor(Math.random() * 1000);
        user = await UserAuth.create({
          googleId: profile.id,
          username: username,
          email: profile.emails ? profile.emails[0].value : `${username}@example.com`,
          name: profile.displayName,
          password: null
        });
        return done(null, user);
      } catch (err) {
         console.error("Ошибка в Google стратегии:", err);
         return done(err);
      }
    }));
  }

  // JWT Strategy for API authentication
  passport.use(new JwtStrategy({
    jwtFromRequest: ExtractJwt.fromAuthHeaderAsBearerToken(),
    // --- Используем БУФЕР, созданный в этом файле ---
    secretOrKey: jwtSecretBufferForPassport
    // -------------------------------------------
  }, async (payload, done) => {
    try {
      // Find user by ID from payload (убедитесь, что поле правильное: id, sub или userId)
      const user = await UserAuth.findByPk(payload.id || payload.sub || payload.userId);
      if (!user) {
        return done(null, false); // Пользователь не найден
      }
      return done(null, user); // Пользователь найден
    } catch (err) {
       console.error("Ошибка в JWT стратегии (поиск пользователя):", err);
       return done(err, false); // Ошибка при поиске
    }
  }));

  // Можно убрать return passport, так как функция теперь просто настраивает глобальный passport
  // return passport;
}; // Функция теперь без аргументов