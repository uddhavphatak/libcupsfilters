//
// Licensed under Apache License v2.0.  See the file "LICENSE" for more
// information.
//
#define CPP 0
#ifndef _CUPS_FILTERS_PDFTOPDF_PPTYPES_H_
#define _CUPS_FILTERS_PDFTOPDF_PPTYPES_H_

#include "pdftopdf-private.h"
#include <cmath> // NAN for cpp

#include <math.h> // NAN for C

// namespace PPTypes {}   TODO?

#if(CPP==0)
typedef enum{
	X,
	Y
} pdftopdf_axis_e;

typedef enum{
	CENTER = 0,
    	LEFT = -1,
    	RIGHT = 1,
    	TOP = 1,
    	BOTTOM = -1
} pdftopdf_position_e;
#endif

#if(CPP==1)
enum pdftopdf_axis_e { X, Y };
enum pdftopdf_position_e { // PS order
  CENTER = 0,
  LEFT = -1,
  RIGHT = 1,
  TOP = 1,
  BOTTOM = -1
};
#endif

void _cfPDFToPDFPositionDump(pdftopdf_position_e pos, pdftopdf_doc_t *doc);
void _cfPDFToPDFPositionDump(pdftopdf_position_e pos, pdftopdf_axis_e axis,
			     pdftopdf_doc_t *doc);

#if(CPP==0)
typedef enum
{
	 ROT_0, 
	 ROT_90, 
	 ROT_180, 
	 ROT_270
} pdftopdf_rotation_e;

void _cfPDFToPDFRotationDump(pdftopdf_rotation_e rot, pdftopdf_doc_t *doc);
#endif

#if(CPP==1)
enum pdftopdf_rotation_e { ROT_0, ROT_90, ROT_180, ROT_270 }; // CCW

void _cfPDFToPDFRotationDump(pdftopdf_rotation_e rot, pdftopdf_doc_t *doc);
#endif


/* CONVERT AFTER SEEING THE PDFIO DEPENDENCY.
#if(CPP==0)
pdftopdf_rotation_e operator_sum(pdftopdf_rotation_e lhs, pdftopdf_rotation_e rhs);
pdftopdf_rotation_e operator_subtract(pdftopdf_rotation_e lhs, pdftopdf_rotation_e rhs);
pdftopdf_rotation_e operator_negate(pdftopdf_rotation_e rhs);
#endif
*/

//#if(CPP==1)
pdftopdf_rotation_e operator+(pdftopdf_rotation_e lhs, pdftopdf_rotation_e rhs);
pdftopdf_rotation_e operator-(pdftopdf_rotation_e lhs, pdftopdf_rotation_e rhs);
pdftopdf_rotation_e operator-(pdftopdf_rotation_e rhs);
//pdftopdf_rotation_e operator+=(pdftopdf_rotation_e &lhs,
//			       pdftopdf_rotation_e rhs);
//#endif
#if(CPP==0)
typedef enum
{
	NONE = 0,
	ONE_THIN = 2,
  	ONE_THICK = 3,
  	TWO_THIN = 4,
  	TWO_THICK = 5,
  	ONE = 0x02,
  	TWO = 0x04,
  	THICK = 0x01
} pdftopdf_border_type_e ;
#endif

#if(CPP==1)
enum pdftopdf_border_type_e {
  NONE = 0,
  ONE_THIN = 2,
  ONE_THICK = 3,
  TWO_THIN = 4,
  TWO_THICK = 5,
  ONE = 0x02,
  TWO = 0x04,
  THICK = 0x01
};
#endif


void _cfPDFToPDFBorderTypeDump(pdftopdf_border_type_e border,
			       pdftopdf_doc_t *doc);

#if(CPP==0)
typedef struct
{
	float top, left, right, bottom;
	float width, height;
} _cfPDFToPDFPageRect;

void _cfPDFToPDFPageRect_init(_cfPDFToPDFPageRect *rect) 
{
    rect->top = NAN;
    rect->left = NAN;
    rect->right = NAN;
    rect->bottom = NAN;
    rect->width = NAN;
    rect->height = NAN;
}

void _cfPDFToPDFPageRect_rotate_move(_cfPDFToPDFPageRect *rect, pdftopdf_rotation_e r, float pwidth, float pheight);
void _cfPDFToPDFPageRect_scale(_cfPDFToPDFPageRect *rect, float mult);
void _cfPDFToPDFPageRect_translate(_cfPDFToPDFPageRect *rect, float tx, float ty);
void _cfPDFToPDFPageRect_set(_cfPDFToPDFPageRect *rect, const _cfPDFToPDFPageRect *rhs);
void _cfPDFToPDFPageRect_dump(const _cfPDFToPDFPageRect *rect, pdftopdf_doc_t *doc); 
#endif

#if(CPP==1)
struct _cfPDFToPDFPageRect {
_cfPDFToPDFPageRect()
  : top(NAN),
    left(NAN),
    right(NAN),
    bottom(NAN),
    width(NAN),
    height(NAN) {}
  float top, left, right, bottom; // i.e. margins
  float width, height;

  void rotate_move(pdftopdf_rotation_e r, float pwidth, float pheight);
                   // pwidth original "page size" (i.e. before rotation)
  void scale(float mult);
  void translate(float tx, float ty);

  void set(const _cfPDFToPDFPageRect &rhs); // only for rhs.* != NAN
  void dump(pdftopdf_doc_t *doc) const;
};
#endif

//  bool _cfPDFToPDFParseBorder(const char *val,pdftopdf_border_type_e &ret);
//                              // none, single, ..., double-thick

#endif // !_CUPS_FILTERS_PDFTOPDF_PPTYPES_H_
