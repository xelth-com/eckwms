/**
 * SVG Definitions Module
 * Contains all SVG filters, icons, and flag definitions
 */

import { loadTemplate } from '/core/module-loader.js';

/**
 * Initialize SVG definitions
 * @param {HTMLElement} container - Container to render SVG defs into
 */
export async function init(container) {
  // Load the SVG definitions template
  const html = await loadTemplate('/svg/svg-defs.template.html');
  container.innerHTML = html;
  
  // Initialize SVG-specific functionality
  initSvgFunctionality();
}

/**
 * Initialize SVG-specific functionality
 */
function initSvgFunctionality() {
  // Initialize SVG background elements
  initSvgBackgrounds();
  
  // Set random seed for certain effects
  setSvgSeed();
}

/**
 * Initialize SVG background elements
 */
function initSvgBackgrounds() {
  // Screen dimensions calculation for backgrounds
  let scrWidth, scrHeight;
  if (screen.width < screen.height) {
    scrWidth = scrHeight = Math.floor(screen.width / 5);
  } else {
    scrWidth = scrHeight = Math.floor(screen.height / 3);
  }
  
  // SVG variables
  const seed = Math.floor(Math.random() * 1000);
  const baseFrequency = 0.025;
  const viewBox = '';
  const numOctaves = 10;
  const matrix1 = `
  0 0 0 0 0 
  0 0 0 0 0 
  0 0 0 0 0 
  0.25 0.25 0.25 0.25 0`;

  // Check browser for filter adjustments
  let chrome = true;
  let surfaceScaleDif = 2;
  let elevation = 20;
  let surfaceScaleSpec = 1;
  let specularExponent = 40;
  
  const tempUserAgent = navigator.userAgent.toLowerCase();
  // Объявляем переменную перед использованием
  let tempIndex;
  if ((tempIndex = tempUserAgent.indexOf('chrome/')) > -1) {
    if (tempUserAgent.slice(tempIndex + 7, tempIndex + 10) < 117) {
      surfaceScaleDif = 6;
      elevation = 20;
      surfaceScaleSpec = 4;
      chrome = false;
    }
  }
  if (navigator.userAgent.toLowerCase().indexOf('mac') > -1) {
    surfaceScaleDif = 2;
    elevation = 30;
    surfaceScaleSpec = 4;
    chrome = false;
  }
  if (navigator.userAgent.toLowerCase().indexOf('firefox') > -1) {
    surfaceScaleDif = 4;
    elevation = 38;
    surfaceScaleSpec = 1.5;
    specularExponent = 50;
    chrome = false;
  }

  // Generate SVG background
  const backSvg = `<svg xmlns='http://www.w3.org/2000/svg' ${viewBox} width="${scrWidth}" height="${scrHeight}">
      <filter id='myf1' x='0' y='0' width='100%' height="100%">
          <feTurbulence type="fractalNoise" stitchTiles="stitch" seed="${seed}" baseFrequency="${baseFrequency}" numOctaves="${numOctaves}" />
          <feColorMatrix type="matrix" values="${matrix1}" />
          <feDiffuseLighting lighting-color='#ffffff' surfaceScale="${surfaceScaleDif}" >
              <feDistantLight  azimuth='${Math.floor(Math.random() * 60 + 45)}' elevation='${elevation}' />
          </feDiffuseLighting>
      </filter>
      <g filter="url(#myf1)" >
        <rect x="-1" y="-1" width="${scrWidth + 2}" height="${scrHeight + 2}"  fill="#b0b0b0"/>
      </g>
    </svg>`;

  // Generate button background
  const backSvg2 = `<svg xmlns='http://www.w3.org/2000/svg'  width="200" height="60">
      <filter id='myf2' x='0' y='0' width='100%' height="100%">
          <feTurbulence type="fractalNoise" stitchTiles="stitch" seed="${seed}" baseFrequency="0.015 0.08 " numOctaves="5" />
          
          <feColorMatrix type="matrix" values="${matrix1}" />
          <feMorphology operator="erode" radius="0.9"></feMorphology>
          <feDiffuseLighting lighting-color='#ffebcc' surfaceScale="${surfaceScaleDif}" >
              <feDistantLight  azimuth='45' elevation='${elevation}' />
          </feDiffuseLighting>
          <feMorphology operator="dilate" radius="1 .2"></feMorphology>
      </filter>
      <g filter="url(#myf2)" >
        <rect x="-1" y="-1" width="204" height="63"  fill="#fcefd4"/>
      </g>
    </svg>`;

  // Generate input background
  const backSvg3 = `<svg xmlns='http://www.w3.org/2000/svg'  width="300" height="100">
    <filter id="myf3" x='0' y='0' width='105%' height='105%'>
      <feTurbulence baseFrequency="0.5" numOctaves="2" />
      <feDisplacementMap in="SourceGraphic" scale="6" xChannelSelector="R" yChannelSelector="G" />
      <feGaussianBlur stdDeviation="0.2" />
    </filter>
    <g filter="url(#myf3)">
      <path fill="#bb5131"  d="m235.41,6.47c-9.07,-1.16 -92.92,-6.85 -153.78,11.31c-40.59,11.87 -70.56,24.48 -75.38,38.85c-4.83,14.38 66.38,34.14 108.84,34.59c42.45,0.46 78.13,-2.6 107.53,-8.26c74.22,-18.17 69.9,-38.99 67.24,-44.96c-4.27,-8.36 -36.27,-20.3 -77.12,-22.83c-40.85,-2.52 -46.19,-1.69 -52.58,-2.01c-6.41,-0.31 -8.54,0.49 -14.95,0.8c-6.41,0.31 -12.64,2.39 -20.93,1.32c7.75,-3.12 8.64,-1.81 20.37,-4.24c11.74,-2.39 8.79,-0.56 28.55,-1.41c19.74,-0.89 6.52,0.93 35.17,1.41c28.64,0.48 85.7,10.63 88.1,26.15c2.4,15.53 5.17,32.58 -78.92,52.66c-38.88,4.09 -14.83,6.01 -81.57,6.62c-66.75,0.62 -82.79,-8.42 -99.33,-12.88c-74.22,-22.92 -14.36,-48.72 1.38,-56.88c14.77,-5.78 43.06,-17.55 79.09,-22.16c36.04,-4.61 45.7,-3.84 75.06,-4.42c29.37,-0.62 41.65,0.89 45.12,2.52c3.46,1.63 7.22,4.9 -1.87,3.74l-0.02,0l0.01,0l0,-0.02l0,0.08l0,0.02z"  stroke-width="0"/>
    </g>
    </svg>`;

  // Store SVG definitions globally
  window.backSvg = backSvg;
  window.backSvg2 = backSvg2;
  window.backSvg3 = backSvg3;

  // Apply background to body
  document.body.style.backgroundImage = `url(data:image/svg+xml;charset=utf-8;base64,${btoa(backSvg)})`;

  // Apply background to buttons
  const backButtonImg = `url(data:image/svg+xml;charset=utf-8;base64,${btoa(backSvg2)})`;
  Array.from(document.getElementsByClassName("button")).forEach(element => {
    element.style.backgroundImage = backButtonImg;
  });
}

/**
 * Set random seed for SVG elements with 'seed' class
 */
function setSvgSeed() {
  const seed = Math.floor(Math.random() * 1000);
  Array.from(document.getElementsByClassName("seed")).forEach(element => {
    element.setAttribute('seed', seed);
  });
}

/**
 * Post-initialization tasks
 */
export function postInit() {
  // Additional SVG processing after DOM update
}