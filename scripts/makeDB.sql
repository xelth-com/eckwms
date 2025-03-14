CREATE EXTENSION IF NOT EXISTS "uuid-ossp";


-- Создание таблицы сессий
CREATE TABLE IF NOT EXISTS "session" (
  "sid" VARCHAR(255) NOT NULL PRIMARY KEY,
  "sess" JSON NOT NULL,
  "expire" TIMESTAMP(6) NOT NULL
);

CREATE INDEX IF NOT EXISTS "IDX_session_expire" ON "session" ("expire");

-- Создание таблицы пользователей
CREATE TABLE IF NOT EXISTS "user_auth" (
  "id" UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  "username" VARCHAR(255) NOT NULL UNIQUE,
  "email" VARCHAR(255) NOT NULL UNIQUE,
  "password" VARCHAR(255),
  "googleId" VARCHAR(255) UNIQUE,
  "name" VARCHAR(255),
  "company" VARCHAR(255),
  "phone" VARCHAR(255),
  "street" VARCHAR(255),
  "houseNumber" VARCHAR(255),
  "postalCode" VARCHAR(255),
  "city" VARCHAR(255),
  "country" VARCHAR(255),
  "lastLogin" TIMESTAMP,
  "role" VARCHAR(50) DEFAULT 'user',
  "isActive" BOOLEAN DEFAULT TRUE,
  "createdAt" TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updatedAt" TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Создание таблицы RMA запросов
CREATE TABLE IF NOT EXISTS "rma_requests" (
  "id" UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  "userId" UUID REFERENCES "user_auth"("id"),
  "rmaCode" VARCHAR(255) NOT NULL UNIQUE,
  "orderCode" VARCHAR(255) NOT NULL,
  "status" VARCHAR(50) DEFAULT 'created',
  "company" VARCHAR(255) NOT NULL,
  "person" VARCHAR(255),
  "street" VARCHAR(255) NOT NULL,
  "houseNumber" VARCHAR(255),
  "postalCode" VARCHAR(255) NOT NULL,
  "city" VARCHAR(255) NOT NULL,
  "country" VARCHAR(255) NOT NULL,
  "email" VARCHAR(255) NOT NULL,
  "invoiceEmail" VARCHAR(255),
  "phone" VARCHAR(255),
  "resellerName" VARCHAR(255),
  "devices" JSONB NOT NULL DEFAULT '[]',
  "orderData" JSONB,
  "receivedAt" TIMESTAMP,
  "processedAt" TIMESTAMP,
  "shippedAt" TIMESTAMP,
  "trackingNumber" VARCHAR(255),
  "createdAt" TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updatedAt" TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Создание администратора (опционально)
DO $$
DECLARE
    admin_exists BOOLEAN;
BEGIN
    SELECT EXISTS(SELECT 1 FROM "user_auth" WHERE "role" = 'admin') INTO admin_exists;
    
    IF NOT admin_exists THEN
        -- Пароль 'admin123' хеширован с bcrypt
        INSERT INTO "user_auth" (
            "username", "email", "password", "name", "role"
        ) VALUES (
            'admin', 'admin@example.com', 
            '$2b$10$7JQRIGSzAOqrCPn63RIONOe4DBmQvw0GsrpNRFLEyaEU0GIJvz81.', 
            'Administrator', 'admin'
        );
    END IF;
END
$$;