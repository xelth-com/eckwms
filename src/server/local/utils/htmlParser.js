// utils/htmlParser.js
const { verifyJWT, betrugerCrc } = require('../../../shared/utils/encryption');
const { formatUnixTimestamp, prettyPrintObject, maskObjectFields } = require('./formatUtils');
const { findKnownCode, isBetDirect } = require('./dataInit');

/**
 * Parses the request data and generates appropriate HTML response
 * @param {Object} parsed - Parsed request data
 * @returns {Promise<string>} - HTML content
 */
const parseHtml = async (parsed) => {
    try {
        parsed.text = parsed.text.trim();
        let htmlContentStart = '';
        let htmlContent = '';
        let htmlContent2 = '';
        let htmlContentEnd = '';
        console.log(parsed, global.runOnServer);
        
        if (parsed.jwt) {
            try {
                const payload = verifyJWT(parsed.jwt, global.secretJwt);
                
                console.log('JWT valid:', payload);
                if (Object.hasOwn(payload, 'u')) {
                    if (parsed.dest == 'outputShow' && ((tempBet = findKnownCode(parsed.text)) || (tempBet = isBetDirect(parsed.text)))) {
                        const type = tempBet.slice(0, 1);
                        htmlContentStart = ``;
                        try {
                            switch (type) {
                                case 'i': htmlContent = prettyPrintObject(global.items.get(tempBet)); break;
                                case 'b': htmlContent = prettyPrintObject(global.boxes.get(tempBet)); break;
                                case 'p': htmlContent = prettyPrintObject(global.places.get(tempBet)); break;
                                case 'o': htmlContent = prettyPrintObject(global.orders.get(tempBet)); break;
                                case 'u': htmlContent = prettyPrintObject(global.users.get(tempBet)); break;
                                default: htmlContent = 'Unknown type: ' + tempBet;
                            }
                        } catch {
                            htmlContent = 'nothing found';
                        }
                        htmlContentEnd = ``;
                    } else if (parsed.name == 'startInput') {
                        htmlContentStart = `<div id='outputTable1' class="cellPaper" style="box-shadow: 0px 10px 20px -10px rgba(255, 255, 255, 0.4)">
                        <span><div style="width: 100%; background-color:#BBB; color: white; font-weight: bold; text-align: center; border: none;">Incorrectly filled out RMA requests and other forms</div>
                        <table style="width:100%; word-wrap:break-word;border: none;"> 
                        <tr style="background-color:#BBB"><td style="width:80px;">Box</td><td style="width:120px;">inDate</td><td style="width:80px;">Days</td><td style="width:120px;">outDate</td><td>Customer</td><td>Order</td><td>Contain</td><td style="width:200px;">Shipping</td></tr>
                        `;
                        let backColor = 'DDD';
                        let backColor2 = 'DDD';
                        
                        global.boxes.forEach((element) => {
                            let packIn = false;
                            let packOut = false;
                            let hasOrder = [];
                            
                            element?.loc.forEach((locElement) => {
                                if (locElement[0] == 'p000000000000000030') packIn = locElement[1];
                                if (locElement[0] == 'p000000000000000060') packOut = locElement[1];
                            });
                            
                            if (packIn || packOut || (() => {
                                let temp = false;
                                element?.in.forEach((inElement) => {
                                    if (inElement[0].slice(0, 1) == 'o') {
                                        hasOrder.push(inElement[0]);
                                        packIn = inElement[1];
                                        temp = true;
                                    }
                                });
                                return temp;
                            })()) {
                                if (!hasOrder.length && element.in?.length) {
                                    element.in.forEach((inElement) => {
                                        if (inElement[0].slice(0, 1) == 'o') {
                                            hasOrder.push(inElement[0]);
                                        }
                                    });
                                }
                                
                                const matches = [];
                                const missingInCont = [];
                                let remainingInCont = [];
                                let decl = [];
                                let shippingCode = '';
                                let orderCode = '';
                                let customerName = '';
                                
                                if (hasOrder.length) {
                                    const prefix = "i70000000000"; // Prefix for decl-IDs
                                    
                                    // Process order declarations
                                    hasOrder.forEach((tempOrder) => {
                                        if (global.orders.has(tempOrder)) {
                                            // Store id and desc, add prefix to IDs
                                            decl.push(...global.orders.get(tempOrder).decl?.map(([id, desc]) => [`${prefix}${id}`, desc]) || []);
                                        }
                                    });
                                    
                                    // cont remains unchanged
                                    const cont = element.cont?.map(([id, timestamp]) => id) || [];
                                    
                                    // Count frequency of IDs in cont
                                    const contCount = cont.reduce((acc, id) => {
                                        acc[id] = (acc[id] || 0) + 1;
                                        return acc;
                                    }, {});
                                    
                                    // Check if each decl-ID is present in cont
                                    for (const [id, desc] of decl) {
                                        if (contCount[id]) {
                                            matches.push(id); // Only ID is used for matches
                                            contCount[id] -= 1; // Consider each instance individually
                                            if (contCount[id] === 0) {
                                                delete contCount[id];
                                            }
                                        } else {
                                            missingInCont.push(id); // Only add the missing ID
                                        }
                                    }
                                    
                                    remainingInCont = Object.keys(contCount); // Unused elements in cont
                                }
                                
                                // Set to avoid duplicates
                                const uniqueShippingCodes = new Set();
                                
                                // Extract shipping codes
                                element.brc?.forEach((brcElement, index) => {
                                    const upsRegex = /^1Z[0-9A-Z]{16}$/;
                                    const dpdRegex = /^%[0-9a-zA-Z]{7}(\d{14})\d{6}$/;
                                    const fedExRegex = /^\d{24}$|^\d{34}$/;
                                    const tntRegex = /^\d{4}(\d{9})\d{15}$/;
                                    const dhlExpressRegex = /^JJD\d{9,10}$/;
                                    const dhlPaketRegex = /^\d{12,14}$/;
                                    const dhlGlobalMailRegex = /^GM\d{9}DE$/;
                                    const dhlEcommerceRegex = /^JJ[A-Z0-9]{19}$/;
                                    
                                    let match;
                                    
                                    if (upsRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_UPS`);
                                    } else if (match = brcElement.match(dpdRegex)) {
                                        uniqueShippingCodes.add(`${match[1]}_DPD`);
                                    } else if (match = brcElement.match(tntRegex)) {
                                        uniqueShippingCodes.add(`<a target="_blank" href="https://www.tnt.com/express/en_gb/site/shipping-tools/tracking.html?searchType=con&cons=${match[1]}">${match[1]}_TNT</a>`);
                                    } else if (fedExRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_FedEx`);
                                    } else if (dhlExpressRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_DHLexp`);
                                    } else if (dhlPaketRegex.test(brcElement) && index === 0) {
                                        uniqueShippingCodes.add(`<a target="_blank" href="https://www.dhl.com/de-en/home/tracking/tracking-parcel.html?submit=1&tracking-id=${brcElement}">${brcElement}_DHLPack</a>`);
                                    } else if (dhlGlobalMailRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_DHLmail`);
                                    } else if (dhlEcommerceRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`<a target="_blank" href="https://www.dhl.com/de-en/home/tracking/tracking-ecommerce.html?submit=1&tracking-id=${brcElement}">${brcElement}_DHLecom</a>`);
                                    }
                                });
                                
                                // Join all unique entries as a string
                                shippingCode = Array.from(uniqueShippingCodes).join(' ');
                                
                                const uniqueDev = new Map();
                                
                                element.cont?.forEach(dev => {
                                    const serialNumber = dev[0];
                                    const timestamp = dev[1];
                                    
                                    if (!uniqueDev.has(serialNumber) || uniqueDev.get(serialNumber).timestamp < timestamp) {
                                        uniqueDev.set(serialNumber, { timestamp, dev });
                                    }
                                });
                                
                                customerName = '';
                                
                                // Process RMA orders
                                const lastLink = element.in[element.in.length - 1]?.[0];
                                const lastLinkTime = element.in[element.in.length - 1]?.[1];
                                
                                if (lastLink && /^o[A-Za-z0-9]{3}RMA\d{10}[A-Za-z0-9]{2}$/.test(lastLink)) {
                                    if (backColor2 == 'EEE') backColor2 = 'DDD'; else backColor2 = 'EEE';
                                    orderCode = `<a onclick="return myFetch('${lastLink}', 'showOrder', 'outputShow');">${lastLink.slice(2).replace(/^0*/, '')}</a>`;
                                    if (global.orders.has(lastLink)) customerName = global.orders.get(lastLink).comp;
                                    packIn = lastLinkTime;
                                    
                                    htmlContent2 += generateBoxRow(element, packIn, packOut, customerName, orderCode, uniqueDev, remainingInCont, matches, missingInCont, shippingCode, backColor2, decl);
                                } else {
                                    // Process non-RMA orders
                                    if (element.in?.length) {
                                        element.in.forEach((link) => {
                                            if (isBetDirect(link[0]) && link[0].slice(0, 1) == 'o') {
                                                orderCode = link[0].replace(/^o0+/, '');
                                            } else if (isBetDirect(link[0]) && link[0].slice(0, 1) == 'u') {
                                                if (global.users.has(link[0])) customerName += global.users.get(link[0]).comp + ' ';
                                            } else {
                                                customerName = link[0] + ' ';
                                            }
                                        });
                                    }
                                    
                                    if (backColor == 'EEE') backColor = 'DDD'; else backColor = 'EEE';
                                    htmlContent += generateBoxRow(element, packIn, packOut, customerName, orderCode, uniqueDev, remainingInCont, matches, missingInCont, shippingCode, backColor, decl);
                                }
                            }
                        });
                        
                        // Complete the tables and add pending RMA requests
                        htmlContent += `</table><br></span></div><div id='outputTable2' class="cellPaper" style="box-shadow: 0px 10px 20px -10px rgba(255, 255, 255, 0.4)">
                        <span><div style="width: 100%; background-color:#BBB; color: white; font-weight: bold; text-align: center; border: none;">Boxes with Valid RMA Request</div>
                        
                        <table style="width:100%; word-wrap:break-word;border: none;"> 
                        <tr style="background-color:#BBB"><td style="width:80px;">Box</td><td style="width:120px;">inDate</td><td style="width:80px;">Days</td><td style="width:120px;">outDate</td><td>Customer</td><td>Order</td><td>Contain</td><td style="width:200px;">Shipping</td></tr>
                        `;
                        
                        htmlContent += htmlContent2;
                        htmlContent += `</table><br></span></div>
                        <div id='outputTable3' class="cellPaper" style="box-shadow: 0px 10px 20px -10px rgba(255, 255, 255, 0.4)">
                        <span class="text3"><div style="width: 100%; background-color:#BBB; color: white; font-weight: bold; text-align: center; border: none;">Pre-registered RMA requests</div>
                        
                        <table style="width:100%; word-wrap:break-word;"> 
                        <tr style="background-color:#BBB"><td style="width:120px;">Date</td><td style="width:150px;">RMA Order</td><td>Customer</td><td>Person</td><td>Declatation</td></tr>
                        `;
                        
                        let backColor1 = 'DDD';
                        
                        // Add pending RMA orders
                        global.orders.forEach((element) => {
                            if (element.cont?.length === 0) {
                                let declarate = '';
                                element.decl?.forEach((declElement) => {
                                    declarate += `${declElement[0]} `;
                                });
                                
                                if (backColor1 == 'EEE') backColor1 = 'DDD'; else backColor1 = 'EEE';
                                
                                htmlContent += `<tr style="background-color:#${backColor1}"><td><a onclick="return myFetch('${element.sn[0]}', 'showOrder', 'outputShow');">${formatUnixTimestamp(element.sn[1]).slice(0, 10)}</a></td><td><a onclick="return myFetch('${element.sn[0]}', 'showOrder', 'outputShow');">${element.sn[0].slice(4)}</a></td><td>${element.comp}</td><td>${element.pers}</td><td>${declarate}</td></tr>`;
                            }
                        });
                        
                        htmlContentEnd = `</table><br></span></div>
                        <div id='outputShow'></div>
                        `;
                    }
                } else if (Object.hasOwn(payload, 'r')) {
                    const maskedObj = payload.a != 'p' ? maskObjectFields(global.orders.get('o000' + payload.r), ["comp", "pers", "str", "cem", "iem"]) : global.orders.get('o000' + payload.r);
                    htmlContent = '<div style="width: min-content;">' + prettyPrintObject(maskedObj) + '</div>';
                }
            } catch (error) {
                console.error('Error:', error.message);
                htmlContent = `${error.message}`;
            }
        }
        
        
         if (parsed.name === 'snInput') {
            // Handle serial number input
            if (/^showmealldevices(\d{8})$/i.test(parsed.text)) {
                // Handle special device list command - formatted in status routes
                const response = await fetch(`http://localhost:${process.env.PORT || 3000}/status/serial/${parsed.text}`, {
                    method: 'GET'
                });
                return await response.text();
            } else if (/^\d{7}$/.test(parsed.text) || /^RMA\d{10}[A-Za-z0-9]{2}$/.test(parsed.text)) {
                // Handle serial number or RMA number lookup - formatted in status routes
                const response = await fetch(`http://localhost:${process.env.PORT || 3000}/status/serial/${parsed.text}`, {
                    method: 'GET'
                });
                return await response.text();
            } else if (!parsed.jwt) {
                try {
                    // Check if input is a JWT
                    const payload = verifyJWT(parsed.text.replace(/\s+/g, ''), global.secretJwt);
                    console.log('JWT valid:', payload);
                    
                    if (Object.hasOwn(payload, 'u')) {
                        htmlContent = parsed.text.replace(/\s+/g, '');
                    } else if (Object.hasOwn(payload, 'r')) {
                        htmlContent = parsed.text.replace(/\s+/g, '');
                    }
                } catch (error) {
                    console.error('JWT Error:', error.message);
                    htmlContent = ``;
                }
            }
        } else if (parsed.text.slice(0, 5) === 'parts') {
            // Handle parts request
            const bodyReq = parsed.text.slice(5);
            htmlContentStart = `<span class="text3">
            <table style="width:100%;word-wrap: break-word;"> 
            <tr>
            <td colspan="3" class="cellPaper">Teile für ${bodyReq}</td>               
            <td colspan="4" class="inputPaper"><span class="text2blue">Die gewünschte<wbr> Bestellung</span></td>
            </tr>`;
            htmlContent = '';
            
            // Process upper-level parts
            const upperTemp = global.uppers.get(bodyReq);
            if (upperTemp && Object.hasOwn(upperTemp, 'down')) {
                upperTemp.down.map((va) => {
                    if (va[0] === 'c') {
                        const classTemp = global.classes.get(va[1]);
                        
                        if (classTemp && Object.hasOwn(classTemp, 'down')) {
                            classTemp.down.map((valu) => {
                                if (valu[0] === 'i') {
                                    const itemTemp = global.items.get(valu[1]);
                                    if (itemTemp) {
                                        htmlContent += `             
        <tr>
        <td class="cellPaper" style="width:80px;">${(itemTemp.cl.toString().match(/.{1,4}/g) || []).map(el => el).join('<wbr>')}</td>
        <td class="imgPaper" style="width:160px;"><img loading="lazy" src="${itemTemp.img[0]}"  onclick="fsPic('${itemTemp.img[0]}')" width="160px"></td>
        <td  class="cellPaper" style="width:80px;"><b>${itemTemp.mult.map(el => el[0]).join('<br>')}</b></td>
        <td  class="inputPaper" style="width:80px;"><span class="text2blue"><div id="${itemTemp.cl}" onfocus="onFocusInt('${itemTemp.cl}')" onfocusout="onFocusOutInt('${itemTemp.cl}',${itemTemp.mult.map(el => el[0]).join('<br>')})" contenteditable="true">0</div></span></td>
        <td  class="cellPaper" style="width:80px;"><small>${itemTemp.rel.map(el => el[0] == 'partOf' ? `${el[1]}` : '').join('<br>')}</small></td>
        <td  class="cellPaper" style="width:80px;"><small><b>${betrugerCrc(itemTemp.sn[0].slice(2).replace(/^0*/, ''))}</b><br>${fullPath(itemTemp).join('<br>')}</small></td>
        <td  class="cellPaper"><small><small>${itemTemp.desc.join('<br>')}</small></small></td>
        </tr>`;
                                    }
                                }
                            });
                        }
                    }
                });
            }
            
            htmlContentEnd = `</table><br></span>`;
        }
        
        if (parsed.name === 'rmaForm') {
            const rmaJs = JSON.parse(parsed.text);
            htmlContentStart = `<span class="text3">`;
            htmlContent = ``;
            htmlContentEnd = `<br></span>`;
        }
        
        return htmlContentStart + htmlContent + htmlContentEnd;
    } catch (error) {
        return `
    <html>
        <head><title>Response</title></head>
        <body>
            <h1>Received Data</h1>
            <p>${error.message || 'No data'}</p><br>
            <h1>Error</h1>
            <p>${error}</p>
        </body>
    </html>`;
    }
};

/**
 * Generates a table row for a box with its details
 * @param {Object} element - Box element
 * @param {number} packIn - Pack in timestamp
 * @param {number} packOut - Pack out timestamp
 * @param {string} customerName - Customer name
 * @param {string} orderCode - Order code
 * @param {Map} uniqueDev - Map of unique devices
 * @param {Array} remainingInCont - Remaining items in cont
 * @param {Array} matches - Matched items
 * @param {Array} missingInCont - Missing items
 * @param {string} shippingCode - Shipping code
 * @param {string} backColor - Background color for the row
 * @param {Array} decl - Declarations
 * @returns {string} - HTML table row
 */
function generateBoxRow(element, packIn, packOut, customerName, orderCode, uniqueDev, remainingInCont, matches, missingInCont, shippingCode, backColor, decl) {
    const { formatUnixTimestamp } = require('./formatUtils');
    
    return `<tr style="background-color:#${backColor}"><td><a onclick="return myFetch('${element.sn[0]}', 'showBox', 'outputShow');">#${(element.sn[0].slice(2).replace(/^0*/, ''))}</a></td>
                                    <td>${packIn ? formatUnixTimestamp(packIn).slice(0, 10) : ''}</td><td>${packIn && packOut ? Math.floor((packOut - packIn) / 60 / 60 / 24) : ''}</td>
                                    <td>${packOut ? formatUnixTimestamp(packOut).slice(0, 10) : ''}</td><td>${customerName}</td><td>${orderCode}</td>
                                    <td><a  onclick="toggleInfoRow(this); return false;">${(() => {
                                            if (uniqueDev.size == matches.length && !missingInCont.length && !remainingInCont.length) {
                                                return '<span style="color:green;">' + uniqueDev.size + '</span>';
                                            } else {
                                                return uniqueDev.size + '/<span style="color:red;">' + remainingInCont.length + '_' + matches.length + '_' + missingInCont.length + '</span>';
                                            }
                                        })()}</a></td><td>${shippingCode}</td></tr>
                                
                                    <tr class="infoRow" style="display:none;">
                                    <td colspan="8" style="text-align:left; background-color:#ee85">${(() => {
                                            let itemsHtml = '';
                                            
                                            uniqueDev.forEach(({ dev }) => {
                                                const itemTemp = global.items.get(dev[0]);
                                                if (!itemTemp) return;
                                                
                                                itemsHtml += `<a onclick="return myFetch('${itemTemp.sn[0]}', 'showItem', 'outputShow');">${itemTemp.sn[0].slice(2).replace(/^0*/, '')}</a>: ${decl.find(([entryId, desc]) => entryId === dev[0])?.[1] ?? ''}
                                            <ul>`;
                                                
                                                itemTemp.actn?.forEach(([type, message, timestamp]) => {
                                                    itemsHtml += `<li><strong>${type}:</strong> ${message} (${formatUnixTimestamp(timestamp)})</li>`;
                                                });
                                                
                                                itemsHtml += '</ul>';
                                            });
                                            
                                            return itemsHtml;
                                        })()}</td></tr>`;
}

/**
 * Gets the full path of a box or item
 * @param {Object} boxitem - Box or item
 * @returns {Array} - Array of location paths
 */
function fullPath(boxitem) {
    if (boxitem.loc && boxitem.loc.length > 0) {
        let pathArr = [];
        const lastLoc = boxitem.loc.at(-1); // Get the last position from loc
        const locSn = lastLoc[0]; // Complete key, e.g. "p000000000000000031" or "b000000000000000001"
        
        if (locSn.startsWith('b')) {
            const box = global.boxes.get(locSn); // Find the box with the new SN
            if (box) {
                pathArr = fullPath(box); // Recursion
            }
            pathArr.push(locSn); // Add the current box SN
        }
        
        if (locSn.startsWith('p')) {
            pathArr.push(locSn); // Add the place directly
        }
        
        return pathArr;
    }
    return [];
}

module.exports = {
    parseHtml,
    fullPath
};