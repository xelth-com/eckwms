/**
 * Menu Permissions Module
 * Handles dynamic menu item visibility based on user roles and permissions
 */

// Configuration of menu items with their required permissions
const MENU_ITEMS_CONFIG = [
    {
        id: 'mainMenu1', // Был mainMenu1
        requiredPermission: 'view_ranger2',
        htmlContent: `
        Ranger2
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/OX10.webp" onclick="myFetch('partsOX10')" />
        </div>
      `
    },
    {
        id: 'mainMenu2', // Был mainMenu2
        requiredPermission: 'view_ranger2k',
        htmlContent: `
        Ranger2k
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/BK10.webp" onclick="myFetch('partsBK10')" />
        </div>
      `
    },
    {
        id: 'mainMenu3', // Был mainMenu4
        requiredPermission: 'view_utouch2',
        htmlContent: `
        UTouch 2
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/sm15.webp" onclick="myFetch('partsSM15')" />
        </div>
      `
    },
    {
        id: 'mainMenu4', // Был mainMenu5
        requiredPermission: 'view_cruise2',
        htmlContent: `
        CRUISE 2
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/SM20.webp" onclick="myFetch('partsSM20')" />
        </div>
      `
    },
    {
        id: 'mainMenu5', // Был mainMenu6
        requiredPermission: 'view_t2',
        htmlContent: `
        T2
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/SL20.webp" onclick="myFetch('partsSL20')" />
        </div>
      `
    },
    {
        id: 'mainMenu6', // Был mainMenu7
        requiredPermission: 'view_sm20',
        htmlContent: `
        SM20
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/SL20K.webp" onclick="myFetch('partsSL20K')" />
        </div>
      `
    },
    {
        id: 'mainMenu7', // Был mainMenu8
        requiredPermission: 'view_us20',
        htmlContent: `
        US20
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/US20.webp" onclick="myFetch('partsUS20')" />
        </div>
      `
    },
    {
        id: 'mainMenu8', // Был mainMenu9
        requiredPermission: 'view_ul20',
        htmlContent: `
        UL20
        <div hidden>
          <img class="picBtn" loading="lazy" height="300" src="storage/pics/ul20.webp" onclick="myFetch('partsUL20')" />
        </div>
      `
    },
    {
        id: 'mainMenu9', // Был mainMenu10
        requiredPermission: 'view_customer',
        htmlContent: `
        KUNDE
        <div hidden>
        </div>
      `
    }
];

/**
 * Check if user has a specific permission
 * @param {string} permission - Permission to check
 * @returns {boolean} - Whether user has the permission
 */
function userHasPermission(permission) {
    // In a real application, this would come from user's role/permissions
    // Example implementation - replace with actual permission logic
    const mockUserPermissions = [
        'view_ranger2',
        'view_ranger2k',
        'view_accessories',
        'view_utouch2',
        'view_cruise2',
        'view_t2',
        'view_sm20',
        'view_us20',
        'view_ul20'
    ];

    // Check if user's permissions include the required permission
    return mockUserPermissions.includes(permission);
}

/**
 * Filter and render menu items based on user permissions
 */
export function renderPermittedMenuItems() {
    const mainMenuButtons = document.getElementById('mainMenuButtons');
    if (!mainMenuButtons) return;

    // Sort items to maintain original order while filtering
    const permittedItems = MENU_ITEMS_CONFIG
        .filter(item => userHasPermission(item.requiredPermission))
        .sort((a, b) => {
            // Extract number from id and sort
            const getNum = id => parseInt(id.replace('mainMenu', ''));
            return getNum(a.id) - getNum(b.id);
        });

    // Clear existing menu items
    mainMenuButtons.innerHTML = '';

    // Render permitted items
    permittedItems.forEach(item => {
        const menuElement = createMenuElement(item);
        mainMenuButtons.appendChild(menuElement);
    });
}

/**
 * Create a menu item element dynamically
 * @param {Object} item - Menu item configuration
 * @returns {HTMLDivElement} - Created menu item
 */
function createMenuElement(item) {
    const menuElement = document.createElement('div');
    menuElement.id = item.id;
    menuElement.className = 'mainMenu';

    // Use the configured HTML content
    if (item.htmlContent) {
        menuElement.innerHTML = item.htmlContent;
    }

    // Add event handlers for hover and menu card interactions
    menuElement.addEventListener('mouseenter', () => mainMenuCardOpen(item.id));
    menuElement.addEventListener('mouseleave', () => mainMenuCardClose(item.id));

    return menuElement;
}

// Export functions to global scope for compatibility
window.renderPermittedMenuItems = renderPermittedMenuItems;

// Initialize on module load
renderPermittedMenuItems();