const { Op } = require('sequelize');
const db = require('../models');

/**
 * Runs data retention cleanup based on instance tiers
 */
async function runRetentionPolicy() {
  console.log('[Retention] Starting cleanup job...');
  try {
    // 1. FREE TIER: Delete 'confirmed' scans immediately (Store & Forward model)
    // These scans have been successfully pulled by the local server
    const freeInstances = await db.EckwmsInstance.findAll({
      where: { tier: 'free' },
      attributes: ['id']
    });

    const freeInstanceIds = freeInstances.map(i => i.id);

    if (freeInstanceIds.length > 0) {
      const deletedConfirmed = await db.Scan.destroy({
        where: {
          instance_id: { [Op.in]: freeInstanceIds },
          status: 'confirmed'
        }
      });
      if (deletedConfirmed > 0) console.log(`[Retention] Free Tier: Cleaned ${deletedConfirmed} confirmed scans.`);

      // 2. FREE TIER: Safety net - delete stale buffered scans older than 7 days
      // Prevents disk filling if local server never comes online
      const staleDate = new Date(Date.now() - 7 * 24 * 60 * 60 * 1000);
      const deletedStale = await db.Scan.destroy({
        where: {
          instance_id: { [Op.in]: freeInstanceIds },
          status: 'buffered',
          createdAt: { [Op.lt]: staleDate }
        }
      });
      if (deletedStale > 0) console.log(`[Retention] Free Tier: Cleaned ${deletedStale} stale buffered scans.`);
    }

    // 3. PAID TIER: Currently we keep everything (History/Backup)
    // Can be configured later to archive data older than 1 year, etc.

    console.log('[Retention] Cleanup job completed.');
  } catch (error) {
    console.error('[Retention] Error running cleanup:', error);
  }
}

module.exports = { runRetentionPolicy };
