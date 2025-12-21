const db = require('../../../shared/models/postgresql');
const { Op } = require('sequelize');

/**
 * Tool: Search for items by external attributes (EAN, Serial, Tracking)
 * Now uses the ProductAlias table for persistent memory
 */
const searchInventoryTool = {
    name: 'search_inventory',
    description: 'Search for items or boxes using external codes like EAN, UPC, Manufacturer Serial, or Tracking Number. Returns a list of potential matches from the alias database.',
    parameters: {
        type: 'object',
        properties: {
            query: {
                type: 'string',
                description: 'The code scanned or text to search for.'
            }
        },
        required: ['query']
    },
    execute: async ({ query }) => {
        try {
            console.log(`[SearchInventory] Looking up: ${query}`);

            // Search in ProductAlias table for exact match
            const exactMatches = await db.ProductAlias.findAll({
                where: {
                    external_code: query
                },
                order: [['createdAt', 'DESC']],
                limit: 5
            });

            // Also search for partial matches (fuzzy search)
            const partialMatches = await db.ProductAlias.findAll({
                where: {
                    external_code: { [Op.like]: `%${query}%` }
                },
                order: [['createdAt', 'DESC']],
                limit: 5
            });

            const allMatches = [...new Map(
                [...exactMatches, ...partialMatches].map(item => [item.id, item])
            ).values()];

            if (allMatches.length === 0) {
                console.log(`[SearchInventory] No matches found for: ${query}`);
                return {
                    found: false,
                    message: 'No direct matches found. This might be a new external code.'
                };
            }

            console.log(`[SearchInventory] Found ${allMatches.length} match(es) for: ${query}`);

            return {
                found: true,
                count: allMatches.length,
                matches: allMatches.map(alias => ({
                    external_code: alias.external_code,
                    internal_id: alias.internal_id,
                    type: alias.type,
                    is_verified: alias.is_verified,
                    created_context: alias.created_context,
                    created_at: alias.createdAt
                }))
            };
        } catch (error) {
            console.error('[SearchInventory] Error:', error);
            return { success: false, error: error.message };
        }
    }
};

/**
 * Tool: Link an external code to an internal ID
 * Now persists to the ProductAlias table
 */
const linkCodeTool = {
    name: 'link_code',
    description: 'Link an external code (EAN, Tracking) to an internal object (Item, Box) as an alias. Stores the link in the database for future lookups.',
    parameters: {
        type: 'object',
        properties: {
            internalId: {
                type: 'string',
                description: 'The internal ID (e.g., i7..., b...)'
            },
            externalCode: {
                type: 'string',
                description: 'The external code found on the package (EAN, Tracking, etc.)'
            },
            type: {
                type: 'string',
                enum: ['ean', 'tracking', 'serial', 'manual_link'],
                description: 'The type of the external code'
            },
            context: {
                type: 'string',
                description: 'The operational context (receiving, moving, picking, etc.)'
            },
            isVerified: {
                type: 'boolean',
                description: 'Whether this link has been verified by a human'
            }
        },
        required: ['internalId', 'externalCode']
    },
    execute: async ({ internalId, externalCode, type = 'manual_link', context = 'unknown', isVerified = false }) => {
        try {
            console.log(`[LinkCode] Linking ${externalCode} (${type}) to ${internalId} [context: ${context}]`);

            // Check if this exact link already exists
            const existingAlias = await db.ProductAlias.findOne({
                where: {
                    external_code: externalCode,
                    internal_id: internalId
                }
            });

            if (existingAlias) {
                console.log(`[LinkCode] Alias already exists: ${externalCode} -> ${internalId}`);

                // Update verification status if needed
                if (isVerified && !existingAlias.is_verified) {
                    await existingAlias.update({ is_verified: true });
                    return {
                        success: true,
                        message: `Alias already exists and is now verified: ${externalCode} -> ${internalId}`,
                        updated: true
                    };
                }

                return {
                    success: true,
                    message: `Alias already exists: ${externalCode} -> ${internalId}`,
                    already_exists: true
                };
            }

            // Check if this external code is linked to a DIFFERENT internal ID
            const conflictingAlias = await db.ProductAlias.findOne({
                where: {
                    external_code: externalCode,
                    internal_id: { [Op.ne]: internalId }
                }
            });

            if (conflictingAlias) {
                console.warn(`[LinkCode] CONFLICT: ${externalCode} is already linked to ${conflictingAlias.internal_id}`);
                return {
                    success: false,
                    conflict: true,
                    message: `Warning: ${externalCode} is already linked to ${conflictingAlias.internal_id}. Cannot link to ${internalId}.`,
                    existing_link: {
                        internal_id: conflictingAlias.internal_id,
                        type: conflictingAlias.type,
                        created_at: conflictingAlias.createdAt
                    }
                };
            }

            // Create new alias
            const newAlias = await db.ProductAlias.create({
                external_code: externalCode,
                internal_id: internalId,
                type: type,
                is_verified: isVerified,
                confidence_score: isVerified ? 100 : 50,
                created_context: context
            });

            console.log(`[LinkCode] Created new alias: ${externalCode} -> ${internalId}`);

            return {
                success: true,
                message: `Successfully linked ${externalCode} to ${internalId}`,
                alias_id: newAlias.id,
                created: true
            };

        } catch (error) {
            console.error('[LinkCode] Error:', error);

            // Handle unique constraint violation
            if (error.name === 'SequelizeUniqueConstraintError') {
                return {
                    success: false,
                    message: `This link already exists in the database.`,
                    already_exists: true
                };
            }

            return {
                success: false,
                error: error.message
            };
        }
    }
};

module.exports = { searchInventoryTool, linkCodeTool };
