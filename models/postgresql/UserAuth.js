// models/postgresql/UserAuth.js
const bcrypt = require('bcrypt');

module.exports = (sequelize, DataTypes) => {
  const UserAuth = sequelize.define('UserAuth', {
    id: {
      type: DataTypes.UUID,
      defaultValue: DataTypes.UUIDV4,
      primaryKey: true
    },
    username: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true
    },
    email: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true,
      validate: {
        isEmail: true
      }
    },
    password: {
      type: DataTypes.STRING,
      allowNull: true // Null for OAuth users and initial RMA users
    },
    googleId: {
      type: DataTypes.STRING,
      allowNull: true,
      unique: true
    },
    name: {
      type: DataTypes.STRING,
      allowNull: true
    },
    company: {
      type: DataTypes.STRING,
      allowNull: true
    },
    phone: {
      type: DataTypes.STRING,
      allowNull: true
    },
    street: {
      type: DataTypes.STRING,
      allowNull: true
    },
    houseNumber: {
      type: DataTypes.STRING,
      allowNull: true
    },
    postalCode: {
      type: DataTypes.STRING,
      allowNull: true
    },
    city: {
      type: DataTypes.STRING,
      allowNull: true
    },
    country: {
      type: DataTypes.STRING,
      allowNull: true
    },
    lastLogin: {
      type: DataTypes.DATE,
      allowNull: true
    },
    role: {
      type: DataTypes.STRING,
      defaultValue: 'user',
      validate: {
        isIn: [['admin', 'user', 'rma']]
      }
    },
    userType: {
      type: DataTypes.STRING,
      defaultValue: 'individual',
      validate: {
        isIn: [['individual', 'company']]
      }
    },
    rmaReference: {
      type: DataTypes.STRING,
      allowNull: true,
      unique: true
    },
    isActive: {
      type: DataTypes.BOOLEAN,
      defaultValue: true
    }
  }, {
    timestamps: true,
    tableName: 'user_auth',
    // Hooks for password hashing
    hooks: {
      beforeCreate: async (user) => {
        if (user.password) {
          user.password = await bcrypt.hash(user.password, 10);
        }
      },
      beforeUpdate: async (user) => {
        if (user.changed('password') && user.password) {
          user.password = await bcrypt.hash(user.password, 10);
        }
      }
    }
  });

  // Method to compare passwords
  UserAuth.prototype.comparePassword = async function(candidatePassword) {
    return bcrypt.compare(candidatePassword, this.password);
  };

  // Is this a company user?
  UserAuth.prototype.isCompany = function() {
    return this.userType === 'company' && !!this.company;
  };

  // Is this an RMA user?
  UserAuth.prototype.isRmaUser = function() {
    return this.role === 'rma';
  };

  // Is this an admin?
  UserAuth.prototype.isAdmin = function() {
    return this.role === 'admin';
  };

  return UserAuth;
};