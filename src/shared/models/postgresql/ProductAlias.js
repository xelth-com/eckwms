module.exports = (sequelize, DataTypes) => {
  const ProductAlias = sequelize.define('ProductAlias', {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true
    },
    external_code: {
      type: DataTypes.STRING,
      allowNull: false
    },
    internal_id: {
      type: DataTypes.STRING,
      allowNull: false
    },
    type: {
      type: DataTypes.STRING,
      allowNull: false
    },
    is_verified: {
      type: DataTypes.BOOLEAN,
      defaultValue: false
    },
    confidence_score: {
      type: DataTypes.INTEGER,
      defaultValue: 0
    },
    created_context: {
      type: DataTypes.STRING,
      allowNull: true
    }
  }, {
    tableName: 'product_aliases',
    timestamps: true,
    indexes: [
      { unique: true, fields: ['external_code', 'internal_id'] }
    ]
  });
  return ProductAlias;
};
