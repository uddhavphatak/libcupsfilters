#include "pdfio-tools-private.h"

pdfio_rect_t
_cfPDFToPDFGetMediaBox1(pdfio_obj_t *page) // {{{
{
  pdfio_rect_t mediaBox;
  pdfio_dict_t *page_dict = pdfioObjGetDict(page);
  pdfioDictGetRect(page_dict, "MediaBox", &mediaBox);
  return (mediaBox);
}
// }}}

pdfio_rect_t
_cfPDFToPDFGetCropBox1(pdfio_obj_t *page) // {{{
{
  pdfio_rect_t cropBox;
  pdfio_dict_t *page_dict = pdfioObjGetDict(page);
  if (!pdfioDictGetRect(page_dict, "CropBox", &cropBox)) 
    return _cfPDFToPDFGetMediaBox1(page);
  return cropBox;
} 
// }}}

pdfio_rect_t
_cfPDFToPDFGetBleedBox1(pdfio_obj_t *page) // {{{
{
  pdfio_rect_t bleedBox;
  pdfio_dict_t *page_dict = pdfioObjGetDict(page);
  if (!pdfioDictGetRect(page_dict, "BleedBox", &bleedBox))
    return _cfPDFToPDFGetCropBox1(page);
  return bleedBox;
}
// }}}

pdfio_rect_t
_cfPDFToPDFGetTrimBox(pdfio_obj_t *page) // {{{
{
  pdfio_rect_t trimBox; 
  pdfio_dict_t *page_dict = pdfioObjGetDict(page);
  if (!pdfioDictGetRect(page_dict, "TrimBox", &trimBox)) 
    return _cfPDFToPDFGetCropBox1(page);
  return trimBox;

}
// }}}

pdfio_rect_t
_cfPDFToPDFGetArtBox1(pdfio_obj_t *page) // {{{
{
  pdfio_rect_t artBox;
  pdfio_dict_t *page_dict = pdfioObjGetDict(page);
  if (!pdfioDictGetRect(page_dict, "ArtBox", &artBox)) 
    return _cfPDFToPDFGetCropBox1(page);
  return artBox;
}
// }}}

pdfio_rect_t
_cfPDFToPDFMakeBox(double x1, 
		   double y1, 
		   double x2, 
		   double y2) // {{{
{
  pdfio_rect_t ret;
  ret.x1 = x1;
  ret.y1 = y1;
  ret.x2 = x2;
  ret.y2 = y2;

  return ret;
}
// }}}
