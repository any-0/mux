"""Unrelated cell/cursor/attribute failures must not satisfy baseline evidence."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from witnesses import intended_intensity_failure


class WitnessSignature(unittest.TestCase):
    def test_only_four_bold_losses_are_accepted(self):
        cells=[]
        for x,c in enumerate('BOTH'):
            wanted=[c,'default','default',True,True,False,False,3,'00d7ff']
            got=list(wanted);got[3]=False
            cells.append(dict(row=0,col=x,expected=wanted,actual=got))
        exact=[dict(cells=cells,total_cell_differences=4,expected_cursor=[3,7],actual_cursor=[3,7],expected_hidden=False,actual_hidden=False,expected_shape='underline',actual_shape='underline')]
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            path=root/'witness-styled-intensity-diff.json'
            path.write_text(json.dumps(exact));self.assertTrue(intended_intensity_failure(root))
            for alter in ('cursor','hidden','shape','glyph','dim','color','count'):
                data=copy.deepcopy(exact)
                if alter=='cursor':data[0]['actual_cursor']=[2,7]
                elif alter=='hidden':data[0]['actual_hidden']=True
                elif alter=='shape':data[0]['actual_shape']='block'
                elif alter=='glyph':data[0]['cells'][0]['actual'][0]='X'
                elif alter=='dim':data[0]['cells'][0]['actual'][4]=False
                elif alter=='color':data[0]['cells'][0]['actual'][1]='ff0000'
                else:data[0]['total_cell_differences']=5
                path.write_text(json.dumps(data));self.assertFalse(intended_intensity_failure(root),alter)
