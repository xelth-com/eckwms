const express = require('express');
const router = express.Router();
const { Role, Permission, sequelize } = require('../../../shared/models/postgresql');
const { requireAdmin } = require('../middleware/auth');

// Middleware: Protect all RBAC routes
router.use(requireAdmin);

// GET /permissions - List all available system permissions
router.get('/permissions', async (req, res) => {
    try {
        const permissions = await Permission.findAll();
        res.json(permissions);
    } catch (err) { res.status(500).json({ error: err.message }); }
});

// GET /roles - List all roles with their permissions
router.get('/roles', async (req, res) => {
    try {
        const roles = await Role.findAll({
            include: [{ model: Permission, through: { attributes: [] } }]
        });
        res.json(roles);
    } catch (err) { res.status(500).json({ error: err.message }); }
});

// POST /roles - Create a new dynamic role
router.post('/roles', async (req, res) => {
    try {
        const { name, description, permissions } = req.body;
        // is_system_protected defaults to false for new roles
        const role = await Role.create({ name, description });

        if (permissions && Array.isArray(permissions)) {
            const perms = await Permission.findAll({ where: { slug: permissions } });
            await role.setPermissions(perms);
        }

        const result = await Role.findByPk(role.id, { include: [Permission] });
        res.status(201).json(result);
    } catch (err) { res.status(500).json({ error: err.message }); }
});

// PUT /roles/:id - Update role permissions
router.put('/roles/:id', async (req, res) => {
    try {
        const role = await Role.findByPk(req.params.id);
        if (!role) return res.status(404).json({ error: 'Role not found' });

        // THE RED BUTTON PROTECTION
        if (role.is_system_protected && req.body.name && req.body.name !== role.name) {
            return res.status(403).json({ error: 'Cannot rename a system protected role' });
        }

        if (req.body.description) role.description = req.body.description;
        await role.save();

        if (req.body.permissions && Array.isArray(req.body.permissions)) {
             // If protected, ensure core.admin is not removed if it was present?
             // For now, we trust the Admin/Agent logic, but strictly prevent deletion of the role itself.
            const perms = await Permission.findAll({ where: { slug: req.body.permissions } });
            await role.setPermissions(perms);
        }

        // TODO: Trigger push to all devices with this role?

        const result = await Role.findByPk(role.id, { include: [Permission] });
        res.json(result);
    } catch (err) { res.status(500).json({ error: err.message }); }
});

// DELETE /roles/:id - Delete a role
router.delete('/roles/:id', async (req, res) => {
    try {
        const role = await Role.findByPk(req.params.id);
        if (!role) return res.status(404).json({ error: 'Role not found' });

        // THE RED BUTTON PROTECTION
        if (role.is_system_protected) {
            return res.status(403).json({ error: 'Cannot delete a system protected role (SUPER_ADMIN)' });
        }

        await role.destroy();
        res.json({ success: true });
    } catch (err) { res.status(500).json({ error: err.message }); }
});

module.exports = router;
