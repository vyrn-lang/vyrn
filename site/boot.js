try{
  var r=document.documentElement,t=localStorage.getItem("vyrn.theme");
  if(t==="light"||t==="dark")r.setAttribute("data-theme",t);
  if(sessionStorage.getItem("vyrn.mast")==="compact")r.classList.add("idecompact");
  r.setAttribute("data-js","on")
}catch(e){
  document.documentElement.setAttribute("data-js","on")
}
