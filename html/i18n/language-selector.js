syncLanguageMasks/**
* Sync language masks with current language
* @param {boolean} shouldToggleGroup - Whether to toggle groups if current language is in a different group
*/
function syncLanguageMasks(shouldToggleGroup = true) {
 try {
   const currentLang = getCurrentLanguage();
   console.log(`Synchronizing language masks for ${currentLang}`);
   
   // Close all masks
   const supportedLangs = [
     'en', 'de', 'tr', 'pl', 'fr', 'it', 'es', 'ru', 'ar', 'zh', 'ro', 'hr', 'bg', 
     'hi', 'ja', 'ko', 'nl', 'uk', 'el', 'sr', 'bs', 'pt', 'cs', 'hu', 'sv', 'da', 
     'no', 'fi', 'sk', 'lt', 'lv', 'et', 'sl', 'he', 'mt', 'ga'
   ];
   
   supportedLangs.forEach(lang => {
     const maskElement = document.getElementById(`${lang}Mask`);
     if (maskElement) {
       maskElement.setAttribute("mask", "url(#maskClose)");
     }
   });
   
   // Open current language mask
   const currentMask = document.getElementById(`${currentLang}Mask`);
   if (currentMask) {
     currentMask.setAttribute("mask", "url(#maskOpen)");
     
     // Only attempt to toggle group if explicitly requested
     // and if the DOM elements exist
     if (shouldToggleGroup) {
       const group1 = document.getElementById('langGroup1');
       const group2 = document.getElementById('langGroup2');
       
       if (group1 && group2) {
         try {
           // Check if current language is in group 2
           const group2Languages = Array.from(document.querySelectorAll('#langGroup2 [data-language]'))
             .map(el => el.getAttribute('data-language'));
           
           // Check if current language is in group 1
           const group1Languages = Array.from(document.querySelectorAll('#langGroup1 [data-language]'))
             .map(el => el.getAttribute('data-language'));
           
           // Update display directly without recursive calls
           if (group2Languages.includes(currentLang) && currentLanguageGroup === 1) {
             // Current language is in group 2 but group 1 is shown
             group1.style.display = 'none';
             group2.style.display = 'inline-block';
             currentLanguageGroup = 2;
             console.log('Switched to language group 2');
           } else if (group1Languages.includes(currentLang) && currentLanguageGroup === 2) {
             // Current language is in group 1 but group 2 is shown
             group1.style.display = 'inline-block';
             group2.style.display = 'none';
             currentLanguageGroup = 1;
             console.log('Switched to language group 1');
           }
         } catch (innerError) {
           console.error("Error in group switching logic:", innerError);
         }
       }
     }
   }
 } catch (e) {
   console.error("Error synchronizing language masks:", e);
 }
}

// Предопределить функцию toggleLanguageGroup в глобальной области видимости
window.toggleLanguageGroup = function() {
  console.log("Language group toggle initiated");
  
  // Простая логика переключения
  const group1 = document.getElementById('langGroup1');
  const group2 = document.getElementById('langGroup2');
  
  if (!group1 || !group2) {
    console.error("Language groups not found in DOM");
    return;
  }
  
  if (group1.style.display !== 'none') {
    group1.style.display = 'none';
    group2.style.display = 'inline-block';
    window.currentLanguageGroup = 2;
  } else {
    group1.style.display = 'inline-block';
    group2.style.display = 'none';
    window.currentLanguageGroup = 1;
  }
};