// routes/status.js
const express = require('express');
const router = express.Router();
const { formatUnixTimestamp } = require('../utils/formatUtils');
const { findKnownCode, isBetDirect } = require('../utils/dataInit');

// Handler for serial number queries
router.get('/serial/:code', (req, res) => {
    const code = req.params.code;
    
    // Handle direct device serial number (7 digits)
    if (/^\d{7}$/.test(code)) {
        const deviceKey = 'i70000000000' + code;
        
        if (!global.items.has(deviceKey)) {
            return res.send(`<br><em>Device not found.</em>`);
        }
        
        const device = global.items.get(deviceKey);
        let html = '';
        
        // Display Device Serial Number and Registration Time
        html += `Device SN: <strong>${code}</strong><br>`;
        html += `Registered on: ${formatUnixTimestamp(device.sn[1])}<br><br>`;
        
        // Display Actions (excluding 'note') with formatted dates
        if (device.actn && device.actn.length > 0) {
            html += `Actions:<ul>`;
            device.actn.forEach(action => {
                const actionType = action[0];
                const actionText = action[1];
                const actionTimestamp = action[2];
                
                // Exclude actions of type 'note'
                if (actionType.toLowerCase() !== 'note') {
                    let style = "";
                    
                    // Conditional styling for 'result' actions based on timestamp
                    if (actionType.toLowerCase() === 'result') {
                        if (actionTimestamp > device.sn[1]) {
                            // 'Result' action after registration time - Green
                            style = 'style="color: green;"';
                        } else {
                            // 'Result' action before registration time - Red
                            style = 'style="color: red;"';
                        }
                    }
                    
                    // Capitalize the first letter of the action type
                    const capitalizedType = actionType.charAt(0).toUpperCase() + actionType.slice(1);
                    
                    // Format the action timestamp
                    const actionDate = formatUnixTimestamp(actionTimestamp);
                    
                    // Append the action to the HTML content with appropriate styling
                    if (style) {
                        html += `<li ${style}>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                    } else {
                        html += `<li>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                    }
                }
            });
            html += `</ul><br><br>`;
        } else {
            html += `<em>No actions available.</em><br><br>`;
        }
        
        // Determine the device's current location
        let currentLocID = '';
        if (device.loc && device.loc.length > 0) {
            const lastLoc = device.loc[device.loc.length - 1];
            currentLocID = lastLoc[0];
            const currentLocTimestamp = lastLoc[1];
            html += `Current Location: ${currentLocID} (${formatUnixTimestamp(currentLocTimestamp)})<br>`;
        } else {
            html += `<em>No location information available.</em><br>`;
        }
        
        // Find the box containing the device to check its last location
        let boxCompleted = false;
        global.boxes.forEach(box => {
            if (box.cont && box.cont.some(cont => cont[0] === deviceKey)) {
                if (box.loc && box.loc.length > 0) {
                    const lastBoxLoc = box.loc[box.loc.length - 1][0];
                    if (lastBoxLoc === 'p000000000000000060') {
                        boxCompleted = true;
                    }
                }
            }
        });
        
        // Check if either the device's last location or its box's last location is 'p000000000000000060'
        if (currentLocID === 'p000000000000000060' || boxCompleted) {
            html += `<br><em style="color: green; font-weight: bold;">Device has been sent back to the client by mail.</em>`;
        }
        
        return res.send(html);
    }
    
    // Handle RMA number queries
    if (/^RMA\d{10}[A-Za-z0-9]{2}$/.test(code)) {
        const orderKey = 'o000' + code;
        
        if (!global.orders.has(orderKey)) {
            return res.send(`<br><em>Order not found.</em>`);
        }
        
        const order = global.orders.get(orderKey);
        let html = '';
        
        html += `The RMA contract <b>${code}</b> was submitted on<br>`;
        html += `${formatUnixTimestamp(order.sn[1])}<br><br><br>`;
        
        // Check for declared issues
        if (order.decl && order.decl.length > 0) {
            html += `<div class="declared-issues"><b>Declared Issues:</b><ul>`;
            order.decl.forEach(declaration => {
                const issueID = declaration[0];
                const issueDescription = declaration[1];
                html += `<li><strong>SN:</strong> ${issueID} - ${issueDescription}</li>`;
            });
            html += `</ul></div><br><br>`;
        } else {
            html += `<div class="no-issues"><em>No declared issues.</em></div><br><br>`;
        }
        
        // Initialize a flag to track if contract completion message should be added
        let contractCompleted = false;
        
        // Check for boxes in 'cont'
        if (order.cont && order.cont.length > 0) {
            html += `<div class="received-packages"><b>Received in Package(s):</b><ul>`;
            order.cont.forEach(container => {
                const boxKey = container[0]; // e.g., "b000000000000000109"
                const registrationTime = container[1]; // Unix timestamp
                
                if (global.boxes.has(boxKey)) {
                    const box = global.boxes.get(boxKey);
                    const formattedTime = formatUnixTimestamp(registrationTime);
                    
                    // Inline Formatting: Remove 'b' and leading zeros from box serial number
                    const formattedBoxSN = box.sn[0].replace(/^b0+/, '');
                    
                    html += `<li><strong>Box Number:</strong> ${formattedBoxSN}<br>`;
                    html += `<strong>Registration Time:</strong> ${formattedTime}<br>`;
                    
                    // Check if the box has loc 'p000000000000000060' and its timestamp > RMA submit time
                    if (box.loc && Array.isArray(box.loc)) {
                        // Find the loc entry for 'p000000000000000060'
                        const locEntry = box.loc.find(loc => loc[0] === 'p000000000000000060');
                        
                        if (locEntry && locEntry[1] > order.sn[1]) {
                            contractCompleted = true;
                        }
                    }
                    
                    // List devices in the box
                    if (box.cont && box.cont.length > 0) {
                        html += `<strong>Registered Devices:</strong><ul>`;
                        box.cont.forEach(device => {
                            let deviceSN = device[0]; // e.g., "i700000000002113897"
                            const deviceTime = device[1];
                            const deviceFormattedTime = formatUnixTimestamp(deviceTime);
                            
                            // Inline Formatting: Extract last 7 characters if deviceSN starts with 'i7'
                            if (deviceSN.startsWith('i7') && deviceSN.length >= 7) {
                                deviceSN = deviceSN.slice(-7);
                            }
                            
                            html += `<li>Device SN: <strong>${deviceSN}</strong><br>Registered on: ${deviceFormattedTime}`;
                            
                            // Retrieve and display action details from items map
                            if (global.items.has(device[0])) { // Using original device SN for lookup
                                const item = global.items.get(device[0]);
                                if (item.actn && item.actn.length > 0) {
                                    html += `<br>Actions:<ul>`;
                                    item.actn.forEach(action => {
                                        const actionType = action[0];
                                        const actionText = action[1];
                                        const actionTimestamp = action[2]; // Unix timestamp of the action
                                        
                                        // Exclude 'note' actions
                                        if (actionType.toLowerCase() !== 'note') {
                                            let style = "";
                                            
                                            // Conditional styling for 'result' actions
                                            if (actionType.toLowerCase() === 'result') {
                                                if (actionTimestamp > order.sn[1]) {
                                                    // Result action after RMA submission time - Green
                                                    style = 'style="color: green;"';
                                                } else {
                                                    // Result action before RMA submission time - Red
                                                    style = 'style="color: red;"';
                                                }
                                            }
                                            
                                            // Capitalize the first letter of the action type
                                            const capitalizedType = actionType.charAt(0).toUpperCase() + actionType.slice(1);
                                            
                                            // Formatted date for each Action
                                            const actionDate = formatUnixTimestamp(actionTimestamp);
                                            
                                            // Apply the style if applicable and include the date
                                            if (style) {
                                                html += `<li ${style}>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                                            } else {
                                                html += `<li>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                                            }
                                        }
                                    });
                                    html += `</ul><br><br>`;
                                }
                            }
                            
                            html += `</li>`;
                        });
                        html += `</ul>`;
                    } else {
                        html += `<em>No devices registered in this box.</em>`;
                    }
                    
                    html += `</li>`;
                } else {
                    html += `<li><strong>Box Key:</strong> ${boxKey} - <em>Box details not found.</em></li>`;
                }
            });
            html += `</ul></div>`;
        } else {
            html += `<div class="no-packages"><em>No packages associated with this order.</em></div>`;
        }
        
        // Append the completion message if the condition is met
        if (contractCompleted) {
            html += `<br><br><em style="color: green; font-weight: bold;">Contract completed and sent back to the client by mail. For detailed information, use the login you received when creating the RMA form.</em>`;
        }
        
        return res.send(html);
    }
    
    // Handle special status query commands
    if (/^showmealldevices(\d{8})$/i.test(code)) {
        // Extract the date part from the command
        const matches = code.match(/^showmealldevices(\d{8})$/i);
        if (!matches) {
            return res.status(400).send("Command does not match the expected format.");
        }
        
        const dateStr = matches[1]; // YYYYMMDD
        
        // Parse the date string into components
        const year = parseInt(dateStr.substring(0, 4), 10);
        const month = parseInt(dateStr.substring(4, 6), 10) - 1; // Months are 0-indexed in JavaScript Date
        const day = parseInt(dateStr.substring(6, 8), 10);
        
        // Validate the parsed date
        if (isNaN(year) || isNaN(month) || isNaN(day)) {
            return res.status(400).send("Invalid date format.");
        }
        
        // Create a Date object and convert it to Unix timestamp (seconds)
        const date = new Date(year, month, day, 0, 0, 0);
        const timestamp = Math.floor(date.getTime() / 1000);
        
        // Initialize an array to hold matching device keys
        let matchingDeviceKeys = [];
        
        // Iterate through all items to find 'i7' devices created after the specified date
        global.items.forEach((device, key) => {
            if (
                key.startsWith('i7') &&
                device.sn &&
                device.sn.length > 1 &&
                device.sn[1] > timestamp
            ) {
                matchingDeviceKeys.push(key);
            }
        });
        
        // Build HTML content based on matching devices
        let html = '';
        
        if (matchingDeviceKeys.length > 0) {
            html += `<b>Devices created after ${formatUnixTimestamp(timestamp)}:</b><br>`;
            html += `
                <table border="1" cellspacing="0" cellpadding="5">
                    <thead>
                        <tr>
                            <th>Device SN</th>
                            <th>Registered On</th>
                            <th>Returned On</th>
                        </tr>
                    </thead>
                    <tbody>
            `;
            
            matchingDeviceKeys.forEach(deviceKey => {
                const device = global.items.get(deviceKey);
                let returned = false;
                let returnedTimestamp = null;
                
                // Check the last location of the device
                if (device.loc && device.loc.length > 0) {
                    const lastDeviceLoc = device.loc[device.loc.length - 1];
                    const lastLocCode = lastDeviceLoc[0];
                    const lastLocTime = lastDeviceLoc[1];
                    
                    if (lastLocCode === 'p000000000000000060') {
                        returned = true;
                        returnedTimestamp = lastLocTime;
                    } else if (lastLocCode.startsWith('b')) {
                        // If last location starts with 'b', it's a box. Check the box's last location.
                        const box = global.boxes.get(lastLocCode);
                        if (box && box.loc && box.loc.length > 0) {
                            const lastBoxLoc = box.loc[box.loc.length - 1];
                            if (lastBoxLoc[0] === 'p000000000000000060') {
                                returned = true;
                                returnedTimestamp = lastBoxLoc[1];
                            }
                        }
                    }
                }
                
                // Calculate days since registration
                const currentTime = Math.floor(Date.now() / 1000); // Current Unix timestamp in seconds
                const daysSinceRegistration = Math.floor((currentTime - device.sn[1]) / 86400); // 86400 seconds in a day
                
                // Determine font color
                const fontColor = getFontColor(daysSinceRegistration, returned);
                
                // Format dates
                const registeredOn = formatUnixTimestamp(device.sn[1]);
                const returnedOn = returned
                    ? formatUnixTimestamp(returnedTimestamp)
                    : 'Not Returned';
                
                // Add a table row with the determined font color
                html += `
                    <tr>
                        <td style="${fontColor}"><strong>${device.sn[0].slice(-7)}</strong></td>
                        <td style="${fontColor}">${registeredOn}</td>
                        <td style="${fontColor}">${returnedOn}</td>
                    </tr>
                `;
            });
            
            html += `
                    </tbody>
                </table>
            `;
        } else {
            html += `<em>No 'i7' devices found created after ${formatUnixTimestamp(timestamp)}.</em>`;
        }
        
        return res.send(html);
    }
    
    // If no matching format, return error
    return res.status(400).send("Invalid code format");
});

// Helper function to determine font color based on days and return status
function getFontColor(daysSinceRegistration, returned) {
    if (returned) {
        return 'color: green;';
    } else {
        if (daysSinceRegistration <= 0) {
            return 'color: blue;';
        } else if (daysSinceRegistration >= 30) { // 30 days threshold
            return 'color: red;';
        } else {
            // Smooth gradient from red to blue
            const ratio = daysSinceRegistration / 30;
            const red = Math.floor(255 * (1 - ratio));
            const blue = Math.floor(255 * ratio);
            return `color: rgb(${red}, 0, ${blue});`;
        }
    }
}

module.exports = router;