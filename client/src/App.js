import React, { useState, lazy, Suspense } from 'react';

// Используем ленивую загрузку для RMA формы
const RMAForm = lazy(() => import('./components/RMAForm'));

const App = () => {
  const [currentPage, setCurrentPage] = useState('home');
  
  const navigateTo = (page) => {
    setCurrentPage(page);
  };
  
  return (
    <>
      <div className="header-logo">M3mobile</div>
      
      {currentPage === 'home' && (
        <div className="max-w-3xl mx-auto p-6 bg-white rounded-lg shadow-lg mt-20">
          <h2 className="text-2xl font-bold mb-6 text-gray-800">Welcome to M3 Mobile Service Portal</h2>
          <p className="mb-4">Use our service portal to create RMA requests and track your repairs.</p>
          
          <button 
            onClick={() => navigateTo('rma')}
            className="py-2 px-6 bg-blue-600 text-white rounded-md hover:bg-blue-700"
          >
            RMA Repair Request
          </button>
        </div>
      )}
      
      {currentPage === 'rma' && (
        <Suspense fallback={<div className="text-center p-10">Loading RMA Form...</div>}>
          <RMAForm onBackClick={() => navigateTo('home')} />
        </Suspense>
      )}
      
      <footer className="text-center p-4 text-white">
        <span>Copyright © 2024 M3 Mobile GmbH. All rights reserved.</span>
      </footer>
    </>
  );
};

export default App;